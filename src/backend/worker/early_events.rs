//! Buffers reactions and reads that arrive before their message.
//!
//! A phone sends receipts and reactions while messages are still being
//! transferred or decrypted; history sync and reconnections also deliver
//! updates out of order.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::model::Delivery;

const MAX_EARLY_EVENTS: usize = 512;
const MAX_EARLY_AGE: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EarlyReaction {
    pub chat: String,
    pub target: String,
    pub sender: String,
    pub from_me: bool,
    pub emoji: String,
    pub arrived_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EarlyReceipt {
    pub chat: String,
    pub target: String,
    pub status: Delivery,
    pub at: i64,
    pub arrived_at: Instant,
}

#[derive(Default)]
pub(crate) struct EarlyEvents {
    // (chat, target, sender) -> EarlyReaction
    reactions: HashMap<(String, String, String), EarlyReaction>,
    reaction_order: VecDeque<(String, String, String)>,
    // (chat, target) -> EarlyReceipt
    receipts: HashMap<(String, String), EarlyReceipt>,
    receipt_order: VecDeque<(String, String)>,
}

impl EarlyEvents {
    pub fn push_reaction(
        &mut self,
        chat: &str,
        target: &str,
        sender: &str,
        from_me: bool,
        emoji: &str,
    ) {
        self.prune_reactions();
        let key = (chat.to_owned(), target.to_owned(), sender.to_owned());
        if !self.reactions.contains_key(&key) {
            if self.reactions.len() >= MAX_EARLY_EVENTS
                && let Some(oldest) = self.reaction_order.pop_front()
            {
                self.reactions.remove(&oldest);
            }
            self.reaction_order.push_back(key.clone());
        }
        self.reactions.insert(
            key,
            EarlyReaction {
                chat: chat.to_owned(),
                target: target.to_owned(),
                sender: sender.to_owned(),
                from_me,
                emoji: emoji.to_owned(),
                arrived_at: Instant::now(),
            },
        );
    }

    /// Takes all early reactions waiting for `(chat, target)`.
    pub fn take_reactions(&mut self, chat: &str, target: &str) -> Vec<EarlyReaction> {
        self.prune_reactions();
        let keys_to_remove: Vec<_> = self
            .reactions
            .keys()
            .filter(|(c, t, _)| c == chat && t == target)
            .cloned()
            .collect();

        let mut matching = Vec::new();
        for key in keys_to_remove {
            if let Some(reaction) = self.reactions.remove(&key) {
                matching.push(reaction);
            }
        }
        matching
    }

    pub fn push_receipt(&mut self, chat: &str, target: &str, status: Delivery, at: i64) {
        self.prune_receipts();
        let key = (chat.to_owned(), target.to_owned());
        if let Some(existing) = self.receipts.get_mut(&key) {
            if status > existing.status {
                existing.status = status;
                existing.at = at;
                existing.arrived_at = Instant::now();
            }
            return;
        }
        if self.receipts.len() >= MAX_EARLY_EVENTS
            && let Some(oldest) = self.receipt_order.pop_front()
        {
            self.receipts.remove(&oldest);
        }
        self.receipt_order.push_back(key.clone());
        self.receipts.insert(
            key,
            EarlyReceipt {
                chat: chat.to_owned(),
                target: target.to_owned(),
                status,
                at,
                arrived_at: Instant::now(),
            },
        );
    }

    /// Takes the furthest early receipt waiting for `(chat, target)`.
    pub fn take_receipt(&mut self, chat: &str, target: &str) -> Option<(Delivery, i64)> {
        self.prune_receipts();
        let key = (chat.to_owned(), target.to_owned());
        self.receipts.remove(&key).map(|r| (r.status, r.at))
    }

    /// Remaps any buffered early events when a privacy id is resolved to a canonical chat.
    pub fn remap_chat(&mut self, from_chat: &str, to_chat: &str) {
        if from_chat == to_chat {
            return;
        }
        let reaction_keys: Vec<_> = self
            .reactions
            .keys()
            .filter(|(c, _, s)| c == from_chat || s == from_chat)
            .cloned()
            .collect();
        for old_key in reaction_keys {
            if let Some(mut reaction) = self.reactions.remove(&old_key) {
                if reaction.chat == from_chat {
                    reaction.chat = to_chat.to_owned();
                }
                if reaction.sender == from_chat {
                    reaction.sender = to_chat.to_owned();
                }
                let new_key = (
                    reaction.chat.clone(),
                    reaction.target.clone(),
                    reaction.sender.clone(),
                );
                self.reactions.insert(new_key, reaction);
            }
        }
        for item in &mut self.reaction_order {
            if item.0 == from_chat {
                item.0 = to_chat.to_owned();
            }
            if item.2 == from_chat {
                item.2 = to_chat.to_owned();
            }
        }

        let receipt_keys: Vec<_> = self
            .receipts
            .keys()
            .filter(|(c, _)| c == from_chat)
            .cloned()
            .collect();
        for old_key in receipt_keys {
            if let Some(mut receipt) = self.receipts.remove(&old_key) {
                receipt.chat = to_chat.to_owned();
                let new_key = (receipt.chat.clone(), receipt.target.clone());
                self.receipts.insert(new_key, receipt);
            }
        }
        for item in &mut self.receipt_order {
            if item.0 == from_chat {
                item.0 = to_chat.to_owned();
            }
        }
    }

    fn prune_reactions(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.reaction_order.front() {
            if let Some(item) = self.reactions.get(front) {
                if now.duration_since(item.arrived_at) > MAX_EARLY_AGE {
                    let key = self.reaction_order.pop_front().unwrap();
                    self.reactions.remove(&key);
                    continue;
                }
            } else {
                self.reaction_order.pop_front();
                continue;
            }
            break;
        }
    }

    fn prune_receipts(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.receipt_order.front() {
            if let Some(item) = self.receipts.get(front) {
                if now.duration_since(item.arrived_at) > MAX_EARLY_AGE {
                    let key = self.receipt_order.pop_front().unwrap();
                    self.receipts.remove(&key);
                    continue;
                }
            } else {
                self.receipt_order.pop_front();
                continue;
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_reactions_are_buffered_and_taken_when_message_arrives() {
        let mut early = EarlyEvents::default();
        early.push_reaction("chat1", "msg1", "alice", false, "❤️");
        early.push_reaction("chat1", "msg1", "bob", false, "🔥");
        early.push_reaction("chat1", "msg2", "alice", false, "👍");

        let taken = early.take_reactions("chat1", "msg1");
        assert_eq!(taken.len(), 2);
        assert!(taken.iter().any(|r| r.sender == "alice" && r.emoji == "❤️"));
        assert!(taken.iter().any(|r| r.sender == "bob" && r.emoji == "🔥"));

        // Second take is empty
        assert!(early.take_reactions("chat1", "msg1").is_empty());

        // Other message remains
        let other = early.take_reactions("chat1", "msg2");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].emoji, "👍");
    }

    #[test]
    fn early_receipts_are_buffered_and_upgraded() {
        let mut early = EarlyEvents::default();
        early.push_receipt("chat1", "msg1", Delivery::Delivered, 100);
        early.push_receipt("chat1", "msg1", Delivery::Read, 120);

        let taken = early.take_receipt("chat1", "msg1");
        assert_eq!(taken, Some((Delivery::Read, 120)));
        assert_eq!(early.take_receipt("chat1", "msg1"), None);
    }

    #[test]
    fn early_events_remap_lid_to_phone_number() {
        let mut early = EarlyEvents::default();
        early.push_reaction("user@lid", "msg1", "user@lid", false, "🎉");
        early.push_reaction("group@g.us", "msg2", "user@lid", false, "🔥");
        early.push_receipt("user@lid", "msg1", Delivery::Delivered, 100);

        early.remap_chat("user@lid", "551199999999@s.whatsapp.net");

        let direct_reactions = early.take_reactions("551199999999@s.whatsapp.net", "msg1");
        assert_eq!(direct_reactions.len(), 1);
        assert_eq!(direct_reactions[0].emoji, "🎉");
        assert_eq!(direct_reactions[0].sender, "551199999999@s.whatsapp.net");

        let group_reactions = early.take_reactions("group@g.us", "msg2");
        assert_eq!(group_reactions.len(), 1);
        assert_eq!(group_reactions[0].emoji, "🔥");
        assert_eq!(group_reactions[0].sender, "551199999999@s.whatsapp.net");

        let receipt = early.take_receipt("551199999999@s.whatsapp.net", "msg1");
        assert_eq!(receipt, Some((Delivery::Delivered, 100)));
    }

    #[test]
    fn capacity_limit_evicts_oldest() {
        let mut early = EarlyEvents::default();
        for i in 0..600 {
            early.push_reaction("chat", &format!("msg{i}"), "sender", false, "👍");
        }
        assert_eq!(early.reactions.len(), MAX_EARLY_EVENTS);
        // The first 88 (600 - 512) were evicted
        assert!(early.take_reactions("chat", "msg0").is_empty());
        assert!(!early.take_reactions("chat", "msg599").is_empty());
    }
}
