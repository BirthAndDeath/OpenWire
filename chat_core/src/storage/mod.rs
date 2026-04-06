mod migrations;
mod identity;
mod contact;
mod message;
mod stats;

pub use identity::{Identity, init, init_path, pool, add_identity, get_current_identity, set_current_identity, list_identities, delete_identity, set_private_key, get_private_key, diagnose_private_key_storage};
pub use contact::{Contact, upsert_contact, delete_contact, list_contacts};
pub use message::{Message, add_message, get_message, get_messages, get_last_message, delete_message, add_messages_batch, mark_sent_batch, delete_messages_batch, list_pending, list_failed, mark_sent, mark_pending, mark_failed};