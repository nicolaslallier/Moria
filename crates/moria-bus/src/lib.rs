//! Fin wrapper autour de `lapin` : un exchange topic `moria`, des messages opaques.

use anyhow::{Context, bail};
use futures::{Stream, StreamExt};
use lapin::options::{
    BasicConsumeOptions, BasicPublishOptions, ConfirmSelectOptions, ExchangeDeclareOptions,
    QueueBindOptions, QueueDeclareOptions,
};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind};
use moria_core::inventory::EXCHANGE;
use std::time::Duration;

pub struct Bus {
    conn: Connection,
    /// Canal en mode confirm, dedie aux publications.
    chan: Channel,
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
            .context("connexion AMQP impossible")?;
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
            let c = confirm.await?;
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
        Ok((chan, name))
    }
}
