//! Persistent admin settings on a single B+tree.
//!
//! Mirrors the source app's singleton config tables (`bankTransferSettings`,
//! `notificationSettings`, `landsbankinnGatewaySettings`, `paydaySettings`) plus
//! the keyed `smsTemplates` / `emailTemplates` tables. Each singleton is stored
//! under a fixed key; templates are stored under `sms:<key>` / `email:<key>`.
//!
//! The store is one B+tree file (`settings.db`) under `data/admin/`, guarded by
//! a single mutex — the same single-writer pattern the booking store uses. Values
//! are JSON objects (`akurai_json`), matching the rest of the port.

use std::io;
use std::path::Path;
use std::sync::Mutex;

use akurai_json::Value;
use akurai_storage::BTree;

/// Bank-transfer payout details shown on the bank-transfer checkout page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BankTransferSettings {
    pub bank_name: String,
    pub account_holder: String,
    pub kennitala: String,
    pub account_number: String,
}

/// Admin-copy notification routing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NotificationSettings {
    pub admin_copy_enabled: bool,
    pub admin_email: String,
    pub admin_phone: String,
}

/// The settings store: one B+tree behind a write lock.
pub struct Store {
    inner: Mutex<BTree>,
}

const KEY_BANK: &[u8] = b"bank_transfer";
const KEY_NOTIFY: &[u8] = b"notifications";

impl Store {
    /// Open (creating if absent) the settings tree under `data_dir`.
    pub fn open(data_dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(data_dir)?;
        let tree = BTree::open(data_dir.join("settings.db"))?;
        Ok(Store {
            inner: Mutex::new(tree),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTree> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- bank transfer ----------------------------------------------------

    /// Read the bank-transfer settings, defaulting every field to empty when the
    /// record has never been written (mirrors the source's lazy-default insert).
    pub fn bank_transfer(&self) -> BankTransferSettings {
        let mut t = self.lock();
        match t.get(KEY_BANK).ok().flatten() {
            Some(raw) => parse_bank(&raw),
            None => BankTransferSettings::default(),
        }
    }

    /// Validate and persist the bank-transfer settings. Returns a user-facing
    /// error string when a required field is blank (mirrors `actions.ts`).
    pub fn set_bank_transfer(&self, input: &BankTransferSettings) -> Result<(), String> {
        let bank_name = input.bank_name.trim();
        let account_holder = input.account_holder.trim();
        let kennitala = input.kennitala.trim();
        let account_number = input.account_number.trim();
        if bank_name.is_empty() {
            return Err("Vantar heiti banka".into());
        }
        if account_holder.is_empty() {
            return Err("Vantar viðtakanda".into());
        }
        if kennitala.is_empty() {
            return Err("Vantar kennitölu".into());
        }
        if account_number.is_empty() {
            return Err("Vantar reikningsupplýsingar".into());
        }
        let value = Value::Object(vec![
            ("bankName".into(), Value::Str(bank_name.into())),
            ("accountHolder".into(), Value::Str(account_holder.into())),
            ("kennitala".into(), Value::Str(kennitala.into())),
            ("accountNumber".into(), Value::Str(account_number.into())),
        ]);
        let mut t = self.lock();
        t.insert(KEY_BANK, value.to_json().as_bytes())
            .map_err(io_err)?;
        t.commit().map_err(io_err)
    }

    // ---- notifications ----------------------------------------------------

    /// Read the notification settings, defaulting to disabled + empty.
    pub fn notifications(&self) -> NotificationSettings {
        let mut t = self.lock();
        match t.get(KEY_NOTIFY).ok().flatten() {
            Some(raw) => parse_notify(&raw),
            None => NotificationSettings::default(),
        }
    }

    /// Persist the notification settings. No required fields (mirrors source).
    pub fn set_notifications(&self, input: &NotificationSettings) -> Result<(), String> {
        let value = Value::Object(vec![
            (
                "adminCopyEnabled".into(),
                Value::Bool(input.admin_copy_enabled),
            ),
            (
                "adminEmail".into(),
                Value::Str(input.admin_email.trim().into()),
            ),
            (
                "adminPhone".into(),
                Value::Str(input.admin_phone.trim().into()),
            ),
        ]);
        let mut t = self.lock();
        t.insert(KEY_NOTIFY, value.to_json().as_bytes())
            .map_err(io_err)?;
        t.commit().map_err(io_err)
    }

    // ---- SMS templates ----------------------------------------------------

    /// Read one SMS template body by key, or `None` if unset.
    pub fn sms_template(&self, key: &str) -> Option<String> {
        let mut t = self.lock();
        let storage_key = sms_key(key);
        let raw = t.get(storage_key.as_bytes()).ok().flatten()?;
        let value = akurai_json::parse(&String::from_utf8_lossy(&raw)).ok()?;
        value
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Validate and persist an SMS template body under `key`.
    pub fn set_sms_template(&self, key: &str, body: &str) -> Result<(), String> {
        if key.trim().is_empty() {
            return Err("Vantar lykil".into());
        }
        if body.trim().is_empty() {
            return Err("Vantar texta".into());
        }
        let value = Value::Object(vec![
            ("key".into(), Value::Str(key.into())),
            ("body".into(), Value::Str(body.into())),
        ]);
        let mut t = self.lock();
        t.insert(sms_key(key).as_bytes(), value.to_json().as_bytes())
            .map_err(io_err)?;
        t.commit().map_err(io_err)
    }
}

// ---- plumbing -------------------------------------------------------------

fn sms_key(key: &str) -> String {
    format!("sms:{key}")
}

fn io_err(e: io::Error) -> String {
    format!("storage error: {e}")
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn parse_bank(raw: &[u8]) -> BankTransferSettings {
    match akurai_json::parse(&String::from_utf8_lossy(raw)) {
        Ok(v) => BankTransferSettings {
            bank_name: str_field(&v, "bankName"),
            account_holder: str_field(&v, "accountHolder"),
            kennitala: str_field(&v, "kennitala"),
            account_number: str_field(&v, "accountNumber"),
        },
        Err(_) => BankTransferSettings::default(),
    }
}

fn parse_notify(raw: &[u8]) -> NotificationSettings {
    match akurai_json::parse(&String::from_utf8_lossy(raw)) {
        Ok(v) => NotificationSettings {
            admin_copy_enabled: v
                .get("adminCopyEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            admin_email: str_field(&v, "adminEmail"),
            admin_phone: str_field(&v, "adminPhone"),
        },
        Err(_) => NotificationSettings::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("gsd-admin-settings-{tag}-{nanos}"));
        p
    }

    #[test]
    fn bank_transfer_persists_across_reopen() {
        let dir = temp_dir("bank");
        {
            let store = Store::open(&dir).unwrap();
            // Default is empty before any write.
            assert_eq!(store.bank_transfer(), BankTransferSettings::default());
            store
                .set_bank_transfer(&BankTransferSettings {
                    bank_name: "  Landsbankinn ".into(),
                    account_holder: "Golfsetrið".into(),
                    kennitala: "5501234567".into(),
                    account_number: "0133-26-001234".into(),
                })
                .unwrap();
        }
        // Reopen: the values are durable and trimmed.
        let store = Store::open(&dir).unwrap();
        let s = store.bank_transfer();
        assert_eq!(s.bank_name, "Landsbankinn");
        assert_eq!(s.account_holder, "Golfsetrið");
        assert_eq!(s.kennitala, "5501234567");
        assert_eq!(s.account_number, "0133-26-001234");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bank_transfer_rejects_blank_fields() {
        let dir = temp_dir("bank-blank");
        let store = Store::open(&dir).unwrap();
        let err = store
            .set_bank_transfer(&BankTransferSettings {
                bank_name: "   ".into(),
                account_holder: "x".into(),
                kennitala: "x".into(),
                account_number: "x".into(),
            })
            .unwrap_err();
        assert!(err.contains("banka"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notifications_persist_and_default() {
        let dir = temp_dir("notify");
        {
            let store = Store::open(&dir).unwrap();
            assert_eq!(store.notifications(), NotificationSettings::default());
            store
                .set_notifications(&NotificationSettings {
                    admin_copy_enabled: true,
                    admin_email: "admin@golf.is".into(),
                    admin_phone: "+3545551234".into(),
                })
                .unwrap();
        }
        let store = Store::open(&dir).unwrap();
        let n = store.notifications();
        assert!(n.admin_copy_enabled);
        assert_eq!(n.admin_email, "admin@golf.is");
        assert_eq!(n.admin_phone, "+3545551234");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sms_template_persists_and_validates() {
        let dir = temp_dir("sms");
        let store = Store::open(&dir).unwrap();
        assert!(store.sms_template("booking_confirmed").is_none());
        assert!(store.set_sms_template("booking_confirmed", "  ").is_err());
        assert!(store.set_sms_template("  ", "body").is_err());
        store
            .set_sms_template("booking_confirmed", "Bókun staðfest!")
            .unwrap();
        assert_eq!(
            store.sms_template("booking_confirmed").as_deref(),
            Some("Bókun staðfest!")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
