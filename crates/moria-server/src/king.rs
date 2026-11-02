//! Conversations avec le roi : cadence, portée, question en vol et repli. Pur et synchrone :
//! la tâche du monde l'appelle pendant le tick, l'horloge est passée en paramètre.

use moria_core::king::{
    Ask, Exchange, FALLBACK_EN, FALLBACK_FR, HISTORY_LEN, KingState, Lang, MAX_REPLY_CHARS,
    MAX_TALK_CHARS, QuestId, Reply, king_in_reach,
};
use moria_core::protocol::ServerMsg;
use moria_core::world::Fortress;
use moria_core::{MountainId, TilePos};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Délai après lequel une question sans réponse reçoit le repli (= expiration du message).
pub const KING_TIMEOUT: Duration = Duration::from_secs(70);
/// Écart minimal entre deux questions acceptées d'une même montagne.
pub const MIN_GAP: Duration = Duration::from_secs(3);

/// Ce que le roi sait de la forteresse au moment de la question.
#[derive(Clone, Debug, PartialEq)]
pub struct Context {
    pub state: KingState,
    pub active: Option<QuestId>,
    pub offered: Vec<QuestId>,
}

pub fn context(f: &Fortress) -> Context {
    Context {
        state: KingState {
            resources: f.resources,
            dwarves: f.dwarves.len() as u32,
            depth: f.max_depth,
        },
        active: f.quest.map(|q| q.id),
        offered: crate::quest::offered(f),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    OutOfReach,
    Empty,
    TooLong,
    TooFast,
    Busy,
}

impl Refusal {
    /// Réponse immédiate du roi, sans passer par le bus.
    pub fn text(self, lang: Lang) -> &'static str {
        let (fr, en) = match self {
            Refusal::OutOfReach => (
                "Approche-toi du trône pour parler au roi.",
                "Come closer to the throne to speak to the king.",
            ),
            Refusal::Empty => (
                "Le roi attend que tu parles.",
                "The king waits for you to speak.",
            ),
            Refusal::TooLong => (
                "Le roi lève la main : sois bref.",
                "The king raises a hand: be brief.",
            ),
            Refusal::TooFast => (
                "Le roi pèse encore tes dernières paroles.",
                "The king is still weighing your last words.",
            ),
            Refusal::Busy => (
                "Le roi réfléchit encore à ta question.",
                "The king is still pondering your question.",
            ),
        };
        pick(lang, fr, en)
    }

    pub fn say(self, lang: Lang) -> ServerMsg {
        ServerMsg::KingSay {
            text: self.text(lang).into(),
            offer: None,
        }
    }
}

fn pick(lang: Lang, fr: &'static str, en: &'static str) -> &'static str {
    match lang {
        Lang::Fr => fr,
        Lang::En => en,
    }
}

/// Repli quand le roi ne répond pas (pas de `moria-king`, file pleine, délai dépassé).
pub fn fallback(lang: Lang) -> ServerMsg {
    ServerMsg::KingSay {
        text: pick(lang, FALLBACK_FR, FALLBACK_EN).into(),
        offer: None,
    }
}

struct InFlight {
    seq: u64,
    at: Instant,
    lang: Lang,
    text: String,
}

#[derive(Default)]
struct Conv {
    last: Option<Instant>,
    asking: Option<InFlight>,
    /// En mémoire seulement : perdu au redémarrage, jamais sauvegardé.
    history: VecDeque<Exchange>,
}

pub struct Talks {
    timeout: Duration,
    seq: u64,
    convs: HashMap<MountainId, Conv>,
}

impl Talks {
    pub fn new(timeout: Duration) -> Talks {
        Talks {
            timeout,
            seq: 0,
            convs: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ask(
        &mut self,
        now: Instant,
        m: MountainId,
        hero: TilePos,
        king: TilePos,
        text: &str,
        lang: Lang,
        ctx: Context,
    ) -> Result<Ask, Refusal> {
        if !king_in_reach(king, hero) {
            return Err(Refusal::OutOfReach);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Refusal::Empty);
        }
        if text.chars().count() > MAX_TALK_CHARS {
            return Err(Refusal::TooLong);
        }
        let c = self.convs.entry(m).or_default();
        if c.asking.is_some() {
            return Err(Refusal::Busy);
        }
        if c.last.is_some_and(|t| now.duration_since(t) < MIN_GAP) {
            return Err(Refusal::TooFast);
        }
        self.seq += 1;
        c.last = Some(now);
        c.asking = Some(InFlight {
            seq: self.seq,
            at: now,
            lang,
            text: text.into(),
        });
        Ok(Ask {
            mountain: m,
            seq: self.seq,
            lang,
            text: text.into(),
            history: c.history.iter().cloned().collect(),
            state: ctx.state,
            active: ctx.active,
            offered: ctx.offered,
        })
    }

    /// La réponse du roi, si elle correspond à la question en vol ; l'offre n'est gardée que
    /// si elle figure dans `offered`.
    pub fn reply(&mut self, r: Reply, offered: &[QuestId]) -> Option<ServerMsg> {
        let c = self.convs.get_mut(&r.mountain)?;
        if c.asking.as_ref().is_none_or(|q| q.seq != r.seq) {
            return None;
        }
        let q = c.asking.take()?;
        let text: String = r.text.chars().take(MAX_REPLY_CHARS).collect();
        if c.history.len() == HISTORY_LEN {
            c.history.pop_front();
        }
        c.history.push_back(Exchange {
            player: q.text,
            king: text.clone(),
        });
        Some(ServerMsg::KingSay {
            text,
            offer: r.offer.filter(|o| offered.contains(o)),
        })
    }

    /// La question n'a pas pu partir (pont absent ou plein) : plus rien n'est en vol.
    pub fn cancel(&mut self, m: MountainId) {
        if let Some(c) = self.convs.get_mut(&m) {
            c.asking = None;
        }
    }

    /// Questions restées sans réponse au-delà du délai : leur repli.
    pub fn expired(&mut self, now: Instant) -> Vec<(MountainId, ServerMsg)> {
        let mut out = Vec::new();
        for (&m, c) in &mut self.convs {
            if let Some(q) = c
                .asking
                .take_if(|q| now.duration_since(q.at) >= self.timeout)
            {
                out.push((m, fallback(q.lang)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::Resources;

    const M: MountainId = MountainId(0);
    const KING: TilePos = TilePos { x: 5, y: 5, z: 0 };

    fn ctx() -> Context {
        Context {
            state: KingState {
                resources: Resources::default(),
                dwarves: 3,
                depth: 1,
            },
            active: None,
            offered: QuestId::ALL.to_vec(),
        }
    }

    fn talks() -> (Talks, Instant) {
        (Talks::new(KING_TIMEOUT), Instant::now())
    }

    fn ask(t: &mut Talks, now: Instant, text: &str) -> Result<Ask, Refusal> {
        t.ask(now, M, KING, KING, text, Lang::Fr, ctx())
    }

    fn answer(seq: u64, text: &str, offer: Option<QuestId>) -> Reply {
        Reply {
            mountain: M,
            seq,
            text: text.into(),
            offer,
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn talk_out_of_reach_is_refused_without_calling_the_bus() {
        let (mut t, now) = talks();
        let far = TilePos::new(KING.x + 2, KING.y, KING.z);
        let r = t.ask(now, M, far, KING, "Salut", Lang::Fr, ctx());
        assert_eq!(r, Err(Refusal::OutOfReach));
        // Rien n'est en vol : pas de repli plus tard, et une question à portée passe.
        assert!(t.expired(now + secs(100)).is_empty());
        assert!(ask(&mut t, now, "Salut").is_ok());
    }

    #[test]
    fn empty_and_281_char_text_are_refused() {
        let (mut t, now) = talks();
        assert_eq!(ask(&mut t, now, "   "), Err(Refusal::Empty));
        let long = "é".repeat(MAX_TALK_CHARS + 1);
        assert_eq!(ask(&mut t, now, &long), Err(Refusal::TooLong));
        let ok = "é".repeat(MAX_TALK_CHARS);
        let a = ask(&mut t, now, &ok).unwrap();
        assert_eq!(a.text, ok);
    }

    #[test]
    fn second_talk_within_3s_is_refused() {
        let (mut t, now) = talks();
        let a = ask(&mut t, now, "Un").unwrap();
        assert!(t.reply(answer(a.seq, "Oui", None), &[]).is_some());
        assert_eq!(
            ask(&mut t, now + Duration::from_millis(2900), "Deux"),
            Err(Refusal::TooFast)
        );
        assert!(ask(&mut t, now + secs(3), "Deux").is_ok());
    }

    #[test]
    fn second_talk_while_busy_is_refused() {
        let (mut t, now) = talks();
        ask(&mut t, now, "Un").unwrap();
        assert_eq!(ask(&mut t, now + secs(10), "Deux"), Err(Refusal::Busy));
    }

    #[test]
    fn late_reply_with_old_seq_is_ignored() {
        let (mut t, now) = talks();
        let old = ask(&mut t, now, "Un").unwrap();
        // Repli au délai, puis une nouvelle question.
        assert_eq!(t.expired(now + KING_TIMEOUT).len(), 1);
        let new = ask(&mut t, now + secs(80), "Deux").unwrap();
        assert_ne!(old.seq, new.seq);
        assert_eq!(t.reply(answer(old.seq, "Tard", None), &[]), None);
        // Une montagne inconnue ne passe pas non plus.
        let mut other = answer(new.seq, "Ailleurs", None);
        other.mountain = MountainId(9);
        assert_eq!(t.reply(other, &[]), None);
        assert!(t.reply(answer(new.seq, "À l'heure", None), &[]).is_some());
        // Doublon : la question n'est plus en vol.
        assert_eq!(t.reply(answer(new.seq, "Encore", None), &[]), None);
    }

    #[test]
    fn reply_offer_not_in_catalogue_is_dropped() {
        let (mut t, now) = talks();
        let a = ask(&mut t, now, "Une quête ?").unwrap();
        let r = t.reply(
            answer(a.seq, "Creuse !", Some(QuestId::Depth)),
            &[QuestId::Food],
        );
        assert_eq!(
            r,
            Some(ServerMsg::KingSay {
                text: "Creuse !".into(),
                offer: None
            })
        );
        let a = ask(&mut t, now + secs(5), "Et alors ?").unwrap();
        let r = t.reply(
            answer(a.seq, "Mange !", Some(QuestId::Food)),
            &[QuestId::Food],
        );
        assert_eq!(
            r,
            Some(ServerMsg::KingSay {
                text: "Mange !".into(),
                offer: Some(QuestId::Food)
            })
        );
    }

    #[test]
    fn reply_text_is_truncated() {
        let (mut t, now) = talks();
        let a = ask(&mut t, now, "Parle").unwrap();
        let long = "ä".repeat(MAX_REPLY_CHARS + 50);
        let Some(ServerMsg::KingSay { text, .. }) = t.reply(answer(a.seq, &long, None), &[]) else {
            panic!("pas de réponse");
        };
        assert_eq!(text.chars().count(), MAX_REPLY_CHARS);
    }

    #[test]
    fn question_with_no_reply_gets_fallback_after_70s() {
        let (mut t, now) = talks();
        let a = t.ask(now, M, KING, KING, "Hello", Lang::En, ctx()).unwrap();
        assert!(t.expired(now + secs(69)).is_empty());
        assert_eq!(t.expired(now + KING_TIMEOUT), vec![(M, fallback(Lang::En))]);
        assert!(t.expired(now + secs(200)).is_empty(), "un seul repli");
        assert_eq!(t.reply(answer(a.seq, "Trop tard", None), &[]), None);
        assert_eq!(
            fallback(Lang::En),
            ServerMsg::KingSay {
                text: FALLBACK_EN.into(),
                offer: None
            }
        );
    }

    #[test]
    fn history_keeps_last_six_exchanges() {
        let (mut t, mut now) = talks();
        for i in 0..8 {
            let a = ask(&mut t, now, &format!("q{i}")).unwrap();
            assert_eq!(a.history.len(), i.min(HISTORY_LEN));
            t.reply(answer(a.seq, &format!("r{i}"), None), &[]).unwrap();
            now += secs(5);
        }
        let a = ask(&mut t, now, "fin").unwrap();
        assert_eq!(a.history.len(), HISTORY_LEN);
        assert_eq!(
            a.history[0],
            Exchange {
                player: "q2".into(),
                king: "r2".into()
            }
        );
        assert_eq!(a.history[5].player, "q7");
        // Le contexte part tel quel.
        assert_eq!((a.mountain, a.lang, a.state), (M, Lang::Fr, ctx().state));
        assert_eq!(a.offered, QuestId::ALL.to_vec());
    }

    #[test]
    fn refusals_have_both_languages() {
        for r in [
            Refusal::OutOfReach,
            Refusal::Empty,
            Refusal::TooLong,
            Refusal::TooFast,
            Refusal::Busy,
        ] {
            assert_ne!(r.text(Lang::Fr), r.text(Lang::En));
        }
    }
}
