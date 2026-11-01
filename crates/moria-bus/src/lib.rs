//! Fin wrapper autour de `lapin` : un exchange topic `moria`, des messages opaques.

use anyhow::bail;
use futures::{Stream, StreamExt};
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, BasicQosOptions,
    ConfirmSelectOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind};
use moria_core::inventory::EXCHANGE;
use std::time::Duration;

const PUBLISH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);

/// Retire le `user:motdepasse@` d'une URL pour la journaliser (separe au DERNIER `@` de l'autorite).
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // ponytail: dernier '@' de toute l'URL ; un '@' dans le vhost sur-redacterait (sans risque).
    match rest.rfind('@') {
        Some(i) => format!("{scheme}://{}", &rest[i + 1..]),
        None => url.to_string(),
    }
}

pub struct Bus {
    conn: Connection,
    /// Canal en mode confirm, dedie aux publications.
    chan: Channel,
}

/// Message a acquitter explicitement ; sans `ack`, le broker le redistribue a la fermeture du canal.
pub struct Delivery {
    pub body: Vec<u8>,
    inner: lapin::message::Delivery,
}

impl Delivery {
    pub async fn ack(self) -> anyhow::Result<()> {
        self.inner.ack(BasicAckOptions::default()).await?;
        Ok(())
    }
}

/// Borne l'attente d'une confirmation du broker.
async fn within<T>(
    d: Duration,
    f: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(d, f)
        .await
        .map_err(|_| anyhow::anyhow!("delai depasse ({d:?}) en attendant le broker"))?
}

pub enum Queue<'a> {
    /// File nommee partagee (messages transitoires) ; `latest_only` ne garde que le dernier.
    Shared { name: &'a str, latest_only: bool },
    /// File privee nommee par le serveur, supprimee a la deconnexion.
    Exclusive,
    /// File durable nommee.
    Durable(&'a str),
}

impl Bus {
    /// Une tentative ; declare l'exchange topic durable `moria`.
    pub async fn connect(url: &str) -> anyhow::Result<Bus> {
        let conn = Connection::connect(url, ConnectionProperties::default())
            .await
            .map_err(|e| {
                // lapin peut citer l'URL (mot de passe compris) : on la masque.
                anyhow::anyhow!(
                    "connexion AMQP impossible a {}: {}",
                    redact_url(url),
                    e.to_string().replace(url, &redact_url(url))
                )
            })?;
        let chan = conn.create_channel().await?;
        chan.exchange_declare(
            EXCHANGE.into(),
            ExchangeKind::Topic,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
        chan.confirm_select(ConfirmSelectOptions::default()).await?;
        Ok(Bus { conn, chan })
    }

    /// Reessaie toutes les 2 s jusqu'a reussir.
    pub async fn connect_forever(url: &str) -> Bus {
        loop {
            match Self::connect(url).await {
                Ok(b) => return b,
                Err(e) => {
                    tracing::warn!("RabbitMQ indisponible ({e:#}), nouvel essai dans 2 s");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    /// `durable` : message persistant, attend la confirmation du broker (erreur sur nack ou
    /// si aucune file n'est liee a la cle). Sinon : transitoire, sans attente.
    pub async fn publish(&self, key: &str, body: Vec<u8>, durable: bool) -> anyhow::Result<()> {
        let props = if durable {
            BasicProperties::default().with_delivery_mode(2)
        } else {
            BasicProperties::default()
        };
        let opts = BasicPublishOptions {
            mandatory: durable,
            ..Default::default()
        };
        let confirm = self
            .chan
            .basic_publish(EXCHANGE.into(), key.into(), opts, &body, props)
            .await?;
        if durable {
            let c = within(PUBLISH_CONFIRM_TIMEOUT, async { Ok(confirm.await?) }).await?;
            if c.is_nack() {
                bail!("publication refusee par le broker ({key})");
            }
            if c.take_message().is_some() {
                bail!("publication sans destinataire: aucune file liee a {key}");
            }
        }
        Ok(())
    }

    /// Declare et lie la file, puis consomme (acquittement automatique). Le flux se termine
    /// quand la connexion ou le canal meurt : a l'appelant de se reconnecter.
    pub async fn subscribe(
        &self,
        key: &str,
        q: Queue<'_>,
    ) -> anyhow::Result<impl Stream<Item = Vec<u8>> + Unpin + use<>> {
        let (chan, name) = self.bind(key, q).await?;
        let consumer = chan
            .basic_consume(
                name.as_str().into(),
                "".into(),
                BasicConsumeOptions {
                    no_ack: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await?;
        Ok(consumer
            .take_while(|d| std::future::ready(d.is_ok()))
            .filter_map(|d| std::future::ready(d.ok().map(|d| d.data))))
    }

    /// Comme `subscribe` mais acquittement manuel (`Delivery::ack`), un message a la fois
    /// (prefetch 1) : un message non acquitte est redistribue si le canal se ferme.
    pub async fn subscribe_acked(
        &self,
        key: &str,
        q: Queue<'_>,
    ) -> anyhow::Result<impl Stream<Item = Delivery> + Unpin + use<>> {
        let (chan, name) = self.bind(key, q).await?;
        chan.basic_qos(1, BasicQosOptions::default()).await?;
        let consumer = chan
            .basic_consume(
                name.as_str().into(),
                "".into(),
                BasicConsumeOptions::default(),
                FieldTable::default(),
            )
            .await?;
        Ok(consumer
            .take_while(|d| std::future::ready(d.is_ok()))
            .filter_map(|d| {
                std::future::ready(d.ok().map(|inner| Delivery {
                    body: inner.data.clone(),
                    inner,
                }))
            }))
    }

    /// Declare et lie seulement, sans consommateur.
    pub async fn declare(&self, key: &str, q: Queue<'_>) -> anyhow::Result<()> {
        self.bind(key, q).await.map(|_| ())
    }

    pub fn is_connected(&self) -> bool {
        self.conn.status().connected()
    }

    /// Fermeture propre (arrete aussi les flux).
    pub async fn close(&self) {
        let _ = self.conn.close(200, "fermeture".into()).await;
    }

    /// Canal dedie (une erreur de declaration ne tue pas le canal de publication).
    async fn bind(&self, key: &str, q: Queue<'_>) -> anyhow::Result<(Channel, String)> {
        let chan = self.conn.create_channel().await?;
        let (name, opts, args) = match q {
            Queue::Shared { name, latest_only } => {
                let mut args = FieldTable::default();
                if latest_only {
                    args.insert("x-max-length".into(), AMQPValue::LongInt(1));
                    args.insert(
                        "x-overflow".into(),
                        AMQPValue::LongString("drop-head".into()),
                    );
                }
                // RabbitMQ 4 refuse les files non durables et non exclusives
                // (transient_nonexcl_queues) : la file est durable, ses messages restent transitoires.
                (name, QueueDeclareOptions::durable(), args)
            }
            Queue::Exclusive => (
                "",
                QueueDeclareOptions::exclusive().auto_delete(),
                FieldTable::default(),
            ),
            Queue::Durable(name) => (name, QueueDeclareOptions::durable(), FieldTable::default()),
        };
        let done = async {
            let declared = chan.queue_declare(name.into(), opts, args).await?;
            let name = declared.name().as_str().to_owned();
            chan.queue_bind(
                name.as_str().into(),
                EXCHANGE.into(),
                key.into(),
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await?;
            anyhow::Ok(name)
        }
        .await;
        match done {
            Ok(name) => Ok((chan, name)),
            Err(e) => {
                let _ = chan.close(200, "echec de declaration".into()).await;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn within_errors_when_future_never_completes() {
        let r = within(
            PUBLISH_CONFIRM_TIMEOUT,
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await;
        assert!(r.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn within_passes_through_a_ready_result() {
        let r = within(PUBLISH_CONFIRM_TIMEOUT, async { Ok(7) }).await;
        assert_eq!(r.unwrap(), 7);
    }

    #[test]
    fn redact_url_strips_user_and_password() {
        assert_eq!(
            redact_url("amqp://moria:s3cr3t@rabbitmq:5672/moria"),
            "amqp://rabbitmq:5672/moria"
        );
    }

    #[test]
    fn redact_url_handles_at_sign_in_password() {
        assert_eq!(
            redact_url("amqp://moria:p@ss:w@rd@rabbitmq:5672/moria"),
            "amqp://rabbitmq:5672/moria"
        );
    }

    #[test]
    fn redact_url_leaves_plain_urls_alone() {
        assert_eq!(
            redact_url("amqp://rabbitmq:5672/moria"),
            "amqp://rabbitmq:5672/moria"
        );
        assert_eq!(redact_url("garbage"), "garbage");
        assert_eq!(redact_url(""), "");
    }
}
