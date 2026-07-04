use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use akurai_json::Value;
use akurai_storage::BTree;
use rusqlite::Connection;

fn main() {
    let args: Vec<String> = env::args().collect();
    let old_db = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("golfsetrid.sqlite"));
    let new_dir = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));

    println!("Migrating from {:?} to {:?}", old_db, new_dir);
    let old = Connection::open(&old_db).expect("open old SQLite");

    for sub in &["auth", "booking", "cart", "giftcards", "checkout", "admin"] {
        let _ = std::fs::create_dir_all(new_dir.join(sub));
    }
    for file in &store_files(&new_dir) {
        let _ = std::fs::remove_file(file);
    }

    let data = read_all(&old);
    write_all(&new_dir, &data);
    println!("Migration complete.");
}

// ── Data structs ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct AllData {
    product_categories: Vec<CatRow>,
    products: Vec<ProdRow>,
    packages: Vec<PkgRow>,
    subscriptions: Vec<SubRow>,
    pricing_rules: Vec<PrRow>,
    gift_cards: Vec<GcRow>,
    sms_templates: Vec<SmsRow>,
    email_templates: Vec<EmRow>,
    bank_transfer: Option<BtRow>,
    notifications: Option<NotifRow>,
    landsbankinn: Option<LbRow>,
    payday: Option<PdRow>,
    users: Vec<UsrRow>,
    auth_users: Vec<AuRow>,
    auth_sessions: Vec<SessRow>,
    bookings: Vec<BkRow>,
    booking_users: Vec<BuRow>,
    user_packages: Vec<UpRow>,
    user_subs: Vec<UsRow>,
    sub_members: Vec<SmRow>,
    carts: Vec<CtRow>,
    cart_items: Vec<CiRow>,
    gc_redemptions: Vec<GcrRow>,
    payments: Vec<PayRow>,
    sub_info: HashMap<String, (i64, String, String)>,
}

macro_rules! dr {
    ( $( $name:ident { $( $field:ident : $ty:ty ),* $(,)? } )* ) => {
        $(
            #[derive(Clone)] struct $name { $( $field : $ty ),* }
        )*
    };
}

dr! {
    CatRow  { id: String, name: String, slug: String, description: Option<String>, position: i64, active: bool, created_at: i64, updated_at: i64 }
    ProdRow { name: String, description: Option<String>, price: i64, image_url: Option<String>, category_id: Option<String>, active: bool, position: i64, created_at: i64, updated_at: i64 }
    PkgRow  { name: String, slot_count: i64, price: i64, active: bool }
    SubRow  { name: String, valid_from: String, valid_until: String, daily_limit: i64, price: i64, shareable: bool, max_members: i64, active: bool }
    PrRow   { name: String, start_hour: i64, end_hour: i64, price: i64, active: bool }
    GcRow   { id: String, code: String, amount: i64, balance: i64, currency: String, status: String, expires_at: Option<i64>, delivery_at: Option<i64>, delivered_at: Option<i64>, purchased_by_user_id: Option<String>, cart_id: Option<String>, recipient_email: Option<String>, recipient_phone: Option<String>, recipient_name: Option<String>, message: Option<String>, created_at: i64, updated_at: i64 }
    SmsRow  { key: String, body: String, updated_at: i64 }
    EmRow   { key: String, subject: String, body_html: String, language: String, updated_at: i64 }
    BtRow   { bank_name: String, account_holder: String, kennitala: String, account_number: String, updated_at: i64 }
    NotifRow{ admin_copy_enabled: bool, admin_email: String, admin_phone: Option<String>, updated_at: i64 }
    LbRow   { api_base_url: String, user_id: String, api_key: String, entity_id: String, payment_contract_id: String, interaction_type: String, updated_at: i64 }
    PdRow   { enabled: bool, api_base_url: String, client_id: String, client_secret: String, default_vat_percentage: i64, create_electronic_invoice: bool, send_email: bool, create_claim: bool, due_days: i64, final_due_days: i64, updated_at: i64 }
    UsrRow  { id: String, name: String, email: Option<String>, phone: Option<String>, role: String, fixed_price: Option<i64>, kennitala: Option<String>, payday_customer_id: Option<String>, created_at: i64, updated_at: i64 }
    AuRow   { email: String }
    SessRow { token: String, email: String, created_sec: i64, expires_sec: i64 }
    BkRow   { id: String, user_id: String, starts_at: i64, status: String, payment_type: String, price_paid: Option<i64>, user_package_id: Option<String>, user_subscription_id: Option<String>, notes: Option<String>, created_at: i64, cancelled_at: Option<i64> }
    BuRow   { id: String, fixed_price: Option<i64> }
    UpRow   { id: String, user_id: String, remaining: i64, package_name: String, slot_count: i64 }
    UsRow   { id: String, user_id: String, sub_id: String, valid_from: String, valid_until: String }
    SmRow   { id: String, user_subscription_id: String, user_id: Option<String>, role: String, status: String, invited_phone: Option<String>, invited_at: i64, accepted_at: Option<i64>, removed_at: Option<i64> }
    CtRow   { id: String, user_id: Option<String>, status: String, currency: String, created_at: i64, updated_at: i64 }
    CiRow   { id: String, cart_id: String, itype: String, ref_id: String, name_snapshot: String, unit_price: i64, quantity: i64, metadata: Option<String>, created_at: i64 }
    GcrRow  { id: String, gift_card_id: String, cart_id: Option<String>, payment_id: Option<String>, amount: i64, redeemed_at: i64 }
    PayRow  { id: String, cart_id: Option<String>, user_id: Option<String>, provider: String, provider_ref: String, status: String, amount: i64, currency: String, raw_payload: Option<String>, created_at: i64, updated_at: i64 }
}

// ── Read ─────────────────────────────────────────────────────────────────────

fn store_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in &["auth", "booking", "cart", "giftcards", "checkout", "admin"] {
        let sd = dir.join(sub);
        if sd.is_dir() {
            for e in std::fs::read_dir(&sd).unwrap() {
                let e = e.unwrap();
                if e.path().extension().map_or(false, |x| x == "db") {
                    out.push(e.path());
                }
            }
        }
    }
    out.extend([
        dir.join("collections.db"),
        dir.join("embeddings.db"),
        dir.join("blobs.db"),
    ]);
    out
}

macro_rules! q {
    ($old:expr, $sql:expr, $t:ty, $f:expr) => {
        if let Ok(mut stmt) = $old.prepare($sql) {
            stmt.query_map([], $f)
                .unwrap()
                .filter_map(|r| r.ok())
                .collect::<Vec<$t>>()
        } else {
            Vec::new()
        }
    };
}

fn one<R, F>(old: &Connection, sql: &str, f: F) -> Option<R>
where
    F: FnOnce(&rusqlite::Row) -> rusqlite::Result<R>,
{
    old.prepare(sql)
        .ok()
        .and_then(|mut s| s.query_row([], f).ok())
}

fn read_all(old: &Connection) -> AllData {
    let mut d = AllData::default();

    let au_rows: Vec<(String, String)> = q!(
        old,
        "SELECT id, email FROM user WHERE email IS NOT NULL AND email != ''",
        (String, String),
        |r| Ok((r.get(0)?, r.get(1)?))
    );
    let mut auth_email_by_id: HashMap<String, String> = HashMap::new();
    for (uid, email) in &au_rows {
        auth_email_by_id.insert(uid.clone(), email.clone());
        d.auth_users.push(AuRow {
            email: email.clone(),
        });
    }

    let mut user_email: HashMap<String, String> = HashMap::new();
    let mut pkg_info: HashMap<String, (String, i64)> = HashMap::new();

    d.users = q!(old, "SELECT id,name,email,phone,role,fixed_price,kennitala,payday_customer_id,created_at,updated_at FROM users ORDER BY created_at",
        UsrRow, |r| Ok(UsrRow{id:r.get(0)?,name:r.get(1)?,email:r.get(2)?,phone:r.get(3)?,role:r.get(4)?,fixed_price:r.get(5)?,kennitala:r.get(6)?,payday_customer_id:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?}));
    for u in &d.users {
        if let Some(ref e) = u.email {
            user_email.insert(u.id.clone(), e.clone());
        }
        if !d.auth_users.iter().any(|a| {
            u.email
                .as_ref()
                .map_or(false, |em| a.email.to_lowercase() == em.to_lowercase())
        }) {
            if let Some(ref email) = u.email {
                d.auth_users.push(AuRow {
                    email: email.clone(),
                });
            }
        }
    }

    let pkg_rows: Vec<(String, String, i64)> = q!(
        old,
        "SELECT id,name,slot_count FROM packages",
        (String, String, i64),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    );
    for (id, name, slots) in &pkg_rows {
        pkg_info.insert(id.clone(), (name.clone(), *slots));
    }

    let sub_rows: Vec<(String, i64, String, String)> = q!(
        old,
        "SELECT id,daily_limit,valid_from,valid_until FROM subscriptions",
        (String, i64, String, String),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    );
    for (id, dl, vf, vu) in &sub_rows {
        d.sub_info.insert(id.clone(), (*dl, vf.clone(), vu.clone()));
    }

    d.auth_sessions = q!(
        old,
        "SELECT token,user_id,expires_at,created_at FROM session",
        SessRow,
        |r| {
            let uid: String = r.get(1)?;
            Ok(SessRow {
                token: r.get(0)?,
                email: auth_email_by_id
                    .get(&uid)
                    .or_else(|| user_email.get(&uid))
                    .cloned()
                    .unwrap_or(uid),
                created_sec: r.get::<_, i64>(3)? / 1000,
                expires_sec: r.get::<_, i64>(2)? / 1000,
            })
        }
    );

    d.product_categories = q!(old, "SELECT id,name,slug,description,position,active,created_at,updated_at FROM product_categories ORDER BY position,created_at",
        CatRow, |r| Ok(CatRow{id:r.get(0)?,name:r.get(1)?,slug:r.get(2)?,description:r.get(3)?,position:r.get(4)?,active:r.get(5)?,created_at:r.get(6)?,updated_at:r.get(7)?}));
    d.products = q!(old, "SELECT name,description,price,image_url,category_id,active,position,created_at,updated_at FROM products ORDER BY position,created_at",
        ProdRow, |r| Ok(ProdRow{name:r.get(0)?,description:r.get(1)?,price:r.get(2)?,image_url:r.get(3)?,category_id:r.get(4)?,active:r.get(5)?,position:r.get(6)?,created_at:r.get(7)?,updated_at:r.get(8)?}));
    d.packages = q!(
        old,
        "SELECT name,slot_count,price,active FROM packages ORDER BY name",
        PkgRow,
        |r| Ok(PkgRow {
            name: r.get(0)?,
            slot_count: r.get(1)?,
            price: r.get(2)?,
            active: r.get(3)?
        })
    );
    d.subscriptions = q!(old, "SELECT name,valid_from,valid_until,daily_limit,price,shareable,max_members,active FROM subscriptions ORDER BY name",
        SubRow, |r| Ok(SubRow{name:r.get(0)?,valid_from:r.get(1)?,valid_until:r.get(2)?,daily_limit:r.get(3)?,price:r.get(4)?,shareable:r.get(5)?,max_members:r.get(6)?,active:r.get(7)?}));
    d.pricing_rules = q!(
        old,
        "SELECT name,start_hour,end_hour,price,active FROM pricing_rules ORDER BY id",
        PrRow,
        |r| Ok(PrRow {
            name: r.get(0)?,
            start_hour: r.get(1)?,
            end_hour: r.get(2)?,
            price: r.get(3)?,
            active: r.get(4)?
        })
    );
    d.gift_cards = q!(old, "SELECT id,code,amount,balance,currency,status,expires_at,delivery_at,delivered_at,purchased_by_user_id,cart_id,recipient_email,recipient_phone,recipient_name,message,created_at,updated_at FROM gift_cards ORDER BY created_at",
        GcRow, |r| Ok(GcRow{id:r.get(0)?,code:r.get(1)?,amount:r.get(2)?,balance:r.get(3)?,currency:r.get(4)?,status:r.get(5)?,expires_at:r.get(6)?,delivery_at:r.get(7)?,delivered_at:r.get(8)?,purchased_by_user_id:r.get(9)?,cart_id:r.get(10)?,recipient_email:r.get(11)?,recipient_phone:r.get(12)?,recipient_name:r.get(13)?,message:r.get(14)?,created_at:r.get(15)?,updated_at:r.get(16)?}));
    d.sms_templates = q!(
        old,
        "SELECT key,body,updated_at FROM sms_templates ORDER BY key",
        SmsRow,
        |r| Ok(SmsRow {
            key: r.get(0)?,
            body: r.get(1)?,
            updated_at: r.get(2)?
        })
    );
    d.email_templates = q!(
        old,
        "SELECT key,subject,body_html,language,updated_at FROM email_templates ORDER BY key",
        EmRow,
        |r| Ok(EmRow {
            key: r.get(0)?,
            subject: r.get(1)?,
            body_html: r.get(2)?,
            language: r.get(3)?,
            updated_at: r.get(4)?
        })
    );

    d.bank_transfer = one(old, "SELECT bank_name,account_holder,kennitala,account_number,updated_at FROM bank_transfer_settings LIMIT 1", |r| Ok(BtRow{bank_name:r.get(0)?,account_holder:r.get(1)?,kennitala:r.get(2)?,account_number:r.get(3)?,updated_at:r.get(4)?}));
    d.notifications = one(old, "SELECT admin_copy_enabled,admin_email,admin_phone,updated_at FROM notification_settings LIMIT 1", |r| Ok(NotifRow{admin_copy_enabled:r.get(0)?,admin_email:r.get(1)?,admin_phone:r.get(2)?,updated_at:r.get(3)?}));
    d.landsbankinn = one(old, "SELECT api_base_url,user_id,api_key,entity_id,payment_contract_id,interaction_type,updated_at FROM landsbankinn_gateway_settings LIMIT 1", |r| Ok(LbRow{api_base_url:r.get(0)?,user_id:r.get(1)?,api_key:r.get(2)?,entity_id:r.get(3)?,payment_contract_id:r.get(4)?,interaction_type:r.get(5)?,updated_at:r.get(6)?}));
    d.payday = one(old, "SELECT enabled,api_base_url,client_id,client_secret,default_vat_percentage,create_electronic_invoice,send_email,create_claim,due_days,final_due_days,updated_at FROM payday_settings LIMIT 1", |r| Ok(PdRow{enabled:r.get(0)?,api_base_url:r.get(1)?,client_id:r.get(2)?,client_secret:r.get(3)?,default_vat_percentage:r.get(4)?,create_electronic_invoice:r.get(5)?,send_email:r.get(6)?,create_claim:r.get(7)?,due_days:r.get(8)?,final_due_days:r.get(9)?,updated_at:r.get(10)?}));

    d.bookings = q!(old, "SELECT id,user_id,starts_at,status,payment_type,price_paid,user_package_id,user_subscription_id,notes,created_at,cancelled_at FROM bookings ORDER BY starts_at",
        BkRow, |r| Ok(BkRow{id:r.get(0)?,user_id:r.get(1)?,starts_at:r.get(2)?,status:r.get(3)?,payment_type:r.get(4)?,price_paid:r.get(5)?,user_package_id:r.get(6)?,user_subscription_id:r.get(7)?,notes:r.get(8)?,created_at:r.get(9)?,cancelled_at:r.get(10)?}));
    d.booking_users = q!(old, "SELECT id,fixed_price FROM users", BuRow, |r| Ok(
        BuRow {
            id: r.get(0)?,
            fixed_price: r.get(1)?
        }
    ));
    d.user_packages = q!(
        old,
        "SELECT id,user_id,package_id,remaining FROM user_packages",
        UpRow,
        |r| {
            let pid: String = r.get(2)?;
            let (pn, sc) = pkg_info.get(&pid).cloned().unwrap_or_default();
            Ok(UpRow {
                id: r.get(0)?,
                user_id: r.get(1)?,
                remaining: r.get(3)?,
                package_name: pn,
                slot_count: sc,
            })
        }
    );
    d.user_subs = q!(
        old,
        "SELECT id,user_id,subscription_id,valid_from,valid_until FROM user_subscriptions",
        UsRow,
        |r| Ok(UsRow {
            id: r.get(0)?,
            user_id: r.get(1)?,
            sub_id: r.get(2)?,
            valid_from: r.get(3)?,
            valid_until: r.get(4)?
        })
    );
    d.sub_members = q!(old, "SELECT id,user_subscription_id,user_id,role,status,invited_phone,invited_at,accepted_at,removed_at FROM user_subscription_members",
        SmRow, |r| Ok(SmRow{id:r.get(0)?,user_subscription_id:r.get(1)?,user_id:r.get(2)?,role:r.get(3)?,status:r.get(4)?,invited_phone:r.get(5)?,invited_at:r.get(6)?,accepted_at:r.get(7)?,removed_at:r.get(8)?}));
    d.carts = q!(
        old,
        "SELECT id,user_id,status,currency,created_at,updated_at FROM carts ORDER BY created_at",
        CtRow,
        |r| Ok(CtRow {
            id: r.get(0)?,
            user_id: r.get(1)?,
            status: r.get(2)?,
            currency: r.get(3)?,
            created_at: r.get(4)?,
            updated_at: r.get(5)?
        })
    );
    d.cart_items = q!(old, "SELECT id,cart_id,type,ref_id,name_snapshot,unit_price,quantity,metadata,created_at FROM cart_items ORDER BY created_at", CiRow, |r| Ok(CiRow{id:r.get(0)?,cart_id:r.get(1)?,itype:r.get(2)?,ref_id:r.get(3)?,name_snapshot:r.get(4)?,unit_price:r.get(5)?,quantity:r.get(6)?,metadata:r.get(7)?,created_at:r.get(8)?}));
    d.gc_redemptions = q!(old, "SELECT id,gift_card_id,cart_id,payment_id,amount,redeemed_at FROM gift_card_redemptions ORDER BY redeemed_at", GcrRow, |r| Ok(GcrRow{id:r.get(0)?,gift_card_id:r.get(1)?,cart_id:r.get(2)?,payment_id:r.get(3)?,amount:r.get(4)?,redeemed_at:r.get(5)?}));
    d.payments = q!(old, "SELECT id,cart_id,user_id,provider,provider_ref,status,amount,currency,raw_payload,created_at,updated_at FROM payments ORDER BY created_at", PayRow, |r| Ok(PayRow{id:r.get(0)?,cart_id:r.get(1)?,user_id:r.get(2)?,provider:r.get(3)?,provider_ref:r.get(4)?,status:r.get(5)?,amount:r.get(6)?,currency:r.get(7)?,raw_payload:r.get(8)?,created_at:r.get(9)?,updated_at:r.get(10)?}));

    println!("  Read: {} users, {} auth, {} sessions | {} cats, {} prods, {} pkgs, {} subs, {} prules | {} gc, {} sms, {} email",
        d.users.len(),d.auth_users.len(),d.auth_sessions.len(),d.product_categories.len(),d.products.len(),d.packages.len(),d.subscriptions.len(),d.pricing_rules.len(),d.gift_cards.len(),d.sms_templates.len(),d.email_templates.len());
    println!("  Read: {} bookings, {} upkg, {} usub, {} smem | {} carts, {} citems | {} gcr, {} pay | bt={} notif={} lb={} pd={}",
        d.bookings.len(),d.user_packages.len(),d.user_subs.len(),d.sub_members.len(),d.carts.len(),d.cart_items.len(),d.gc_redemptions.len(),d.payments.len(),d.bank_transfer.is_some(),d.notifications.is_some(),d.landsbankinn.is_some(),d.payday.is_some());
    d
}

// ── Write ────────────────────────────────────────────────────────────────────

fn js(s: &str) -> Value {
    Value::Str(s.into())
}
fn ji(n: i64) -> Value {
    Value::Int(n)
}
fn jb(b: bool) -> Value {
    Value::Bool(b)
}
fn jn() -> Value {
    Value::Null
}
fn joi(n: Option<i64>) -> Value {
    n.map(ji).unwrap_or(jn())
}
fn jos(s: &Option<String>) -> Value {
    s.as_ref().map(|x| js(x)).unwrap_or(jn())
}

fn obj<I: IntoIterator<Item = (String, Value)>>(items: I) -> Value {
    Value::Object(items.into_iter().collect())
}

fn kv(k: &str, v: Value) -> (String, Value) {
    (k.to_string(), v)
}

fn coll_key(name: &str, id: u64) -> Vec<u8> {
    let mut k = format!("coll:{name}:").into_bytes();
    k.extend_from_slice(&id.to_be_bytes());
    k
}
fn coll_seq(name: &str) -> Vec<u8> {
    format!("coll:{name}:_seq").into_bytes()
}

fn parse_date_ms(s: &str) -> i64 {
    if s.len() == 10 && s.chars().filter(|c| *c == '-').count() == 2 {
        let p: Vec<&str> = s.split('-').collect();
        if p.len() == 3 {
            if let (Ok(y), Ok(m), Ok(d)) = (
                p[0].parse::<i64>(),
                p[1].parse::<u64>(),
                p[2].parse::<u64>(),
            ) {
                if m >= 1 && m <= 12 && d >= 1 && d <= 31 {
                    let era = if m <= 2 { y as u64 - 1 } else { y as u64 };
                    let yoe = era - era / 400 * 400;
                    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
                    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                    return ((era / 400 * 146097 + doe - 719468) as i64) * 86_400_000;
                }
            }
        }
    }
    0
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct Stores {
    c: BTree,
    au: BTree,
    as_: BTree,
    bb: BTree,
    bp: BTree,
    bs: BTree,
    bm: BTree,
    bu: BTree,
    cc: BTree,
    ci: BTree,
    gc: BTree,
    gr: BTree,
    cp: BTree,
    cg: BTree,
    ad: BTree,
}

impl Stores {
    fn open(dir: &Path) -> Stores {
        Stores {
            c: BTree::open(dir.join("collections.db")).unwrap(),
            au: BTree::open(dir.join("auth/users.db")).unwrap(),
            as_: BTree::open(dir.join("auth/sessions.db")).unwrap(),
            bb: BTree::open(dir.join("booking/bookings.db")).unwrap(),
            bp: BTree::open(dir.join("booking/user_packages.db")).unwrap(),
            bs: BTree::open(dir.join("booking/user_subscriptions.db")).unwrap(),
            bm: BTree::open(dir.join("booking/user_subscription_members.db")).unwrap(),
            bu: BTree::open(dir.join("booking/users.db")).unwrap(),
            cc: BTree::open(dir.join("cart/carts.db")).unwrap(),
            ci: BTree::open(dir.join("cart/cart_items.db")).unwrap(),
            gc: BTree::open(dir.join("giftcards/gift_cards.db")).unwrap(),
            gr: BTree::open(dir.join("giftcards/gift_card_redemptions.db")).unwrap(),
            cp: BTree::open(dir.join("checkout/payments.db")).unwrap(),
            cg: BTree::open(dir.join("checkout/gift_cards.db")).unwrap(),
            ad: BTree::open(dir.join("admin/settings.db")).unwrap(),
        }
    }
    fn commit_all(&mut self) {
        for t in [
            &mut self.c,
            &mut self.au,
            &mut self.as_,
            &mut self.bb,
            &mut self.bp,
            &mut self.bs,
            &mut self.bm,
            &mut self.bu,
            &mut self.cc,
            &mut self.ci,
            &mut self.gc,
            &mut self.gr,
            &mut self.cp,
            &mut self.cg,
            &mut self.ad,
        ] {
            let _ = t.commit();
        }
    }
    fn set_seq(&mut self, name: &str, id: u64) {
        self.c.insert(&coll_seq(name), &id.to_be_bytes()).unwrap();
    }
    fn wc(&mut self, name: &str, id: u64, val: &Value) {
        self.c
            .insert(&coll_key(name, id), val.to_json().as_bytes())
            .unwrap();
    }
}

fn write_all(dir: &Path, d: &AllData) {
    let mut s = Stores::open(dir);
    let now = now_secs();
    let mut cat_map: HashMap<String, u64> = HashMap::new();

    // product_categories
    for (i, cat) in d.product_categories.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("product_categories", id);
        let mut v = vec![
            kv("id", ji(id as i64)),
            kv("created", ji(now)),
            kv("name", js(&cat.name)),
            kv("slug", js(&cat.slug)),
            kv("position", ji(cat.position)),
            kv("active", jb(cat.active)),
            kv("created_at", ji(cat.created_at)),
            kv("updated_at", ji(cat.updated_at)),
        ];
        if let Some(ref d) = cat.description {
            v.push(kv("description", js(d)));
        }
        s.wc("product_categories", id, &obj(v));
        cat_map.insert(cat.id.clone(), id);
    }

    // products
    for (i, prod) in d.products.iter().enumerate() {
        let id = (d.product_categories.len() + i + 1) as u64;
        s.set_seq("products", id);
        let mut v = vec![
            kv("id", ji(id as i64)),
            kv("created", ji(now)),
            kv("name", js(&prod.name)),
            kv("price", ji(prod.price)),
            kv("active", jb(prod.active)),
            kv("position", ji(prod.position)),
            kv("created_at", ji(prod.created_at)),
            kv("updated_at", ji(prod.updated_at)),
        ];
        if let Some(ref d) = prod.description {
            v.push(kv("description", js(d)));
        }
        if let Some(ref img) = prod.image_url {
            v.push(kv("image_url", js(img)));
        }
        if let Some(ref cid) = prod.category_id {
            if let Some(&nc) = cat_map.get(cid) {
                v.push(kv("category", ji(nc as i64)));
            }
        }
        s.wc("products", id, &obj(v));
    }

    // packages
    for (i, pkg) in d.packages.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("packages", id);
        s.wc(
            "packages",
            id,
            &obj(vec![
                kv("id", ji(id as i64)),
                kv("created", ji(now)),
                kv("name", js(&pkg.name)),
                kv("slot_count", ji(pkg.slot_count)),
                kv("price", ji(pkg.price)),
                kv("active", jb(pkg.active)),
            ]),
        );
    }

    // subscriptions
    for (i, sub) in d.subscriptions.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("subscriptions", id);
        s.wc(
            "subscriptions",
            id,
            &obj(vec![
                kv("id", ji(id as i64)),
                kv("created", ji(now)),
                kv("name", js(&sub.name)),
                kv("valid_from", js(&sub.valid_from)),
                kv("valid_until", js(&sub.valid_until)),
                kv("daily_limit", ji(sub.daily_limit)),
                kv("price", ji(sub.price)),
                kv("shareable", jb(sub.shareable)),
                kv("max_members", ji(sub.max_members)),
                kv("active", jb(sub.active)),
            ]),
        );
    }

    // pricing_rules
    for (i, pr) in d.pricing_rules.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("pricing_rules", id);
        s.wc(
            "pricing_rules",
            id,
            &obj(vec![
                kv("id", ji(id as i64)),
                kv("created", ji(now)),
                kv("name", js(&pr.name)),
                kv("start_hour", ji(pr.start_hour)),
                kv("end_hour", ji(pr.end_hour)),
                kv("price", ji(pr.price)),
                kv("active", jb(pr.active)),
            ]),
        );
    }

    // gift_cards (coll)
    for (i, gc) in d.gift_cards.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("gift_cards", id);
        let mut v = vec![
            kv("id", ji(id as i64)),
            kv("created", ji(now)),
            kv("code", js(&gc.code)),
            kv("amount", ji(gc.amount)),
            kv("balance", ji(gc.balance)),
            kv("currency", js(&gc.currency)),
            kv("status", js(&gc.status)),
            kv("created_at", ji(gc.created_at)),
            kv("updated_at", ji(gc.updated_at)),
        ];
        for (k, val) in [
            ("expires_at", joi(gc.expires_at)),
            ("delivery_at", joi(gc.delivery_at)),
            ("delivered_at", joi(gc.delivered_at)),
            ("recipient_email", jos(&gc.recipient_email)),
            ("recipient_phone", jos(&gc.recipient_phone)),
            ("recipient_name", jos(&gc.recipient_name)),
            ("message", jos(&gc.message)),
        ] {
            if !matches!(val, Value::Null) {
                v.push(kv(k, val));
            }
        }
        s.wc("gift_cards", id, &obj(v));
    }

    // sms_templates
    for (i, tpl) in d.sms_templates.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("sms_templates", id);
        s.wc(
            "sms_templates",
            id,
            &obj(vec![
                kv("id", ji(id as i64)),
                kv("created", ji(now)),
                kv("key", js(&tpl.key)),
                kv("body", js(&tpl.body)),
                kv("updated_at", ji(tpl.updated_at)),
            ]),
        );
    }

    // email_templates
    for (i, tpl) in d.email_templates.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("email_templates", id);
        s.wc(
            "email_templates",
            id,
            &obj(vec![
                kv("id", ji(id as i64)),
                kv("created", ji(now)),
                kv("key", js(&tpl.key)),
                kv("subject", js(&tpl.subject)),
                kv("body_html", js(&tpl.body_html)),
                kv("language", js(&tpl.language)),
                kv("updated_at", ji(tpl.updated_at)),
            ]),
        );
    }

    // settings singletons
    if let Some(ref bt) = d.bank_transfer {
        s.set_seq("bank_transfer_settings", 1);
        s.wc(
            "bank_transfer_settings",
            1,
            &obj(vec![
                kv("id", ji(1)),
                kv("created", ji(now)),
                kv("bank_name", js(&bt.bank_name)),
                kv("account_holder", js(&bt.account_holder)),
                kv("kennitala", js(&bt.kennitala)),
                kv("account_number", js(&bt.account_number)),
                kv("updated_at", ji(bt.updated_at)),
            ]),
        );
    }
    if let Some(ref n) = d.notifications {
        s.set_seq("notification_settings", 1);
        let mut v = vec![
            kv("id", ji(1)),
            kv("created", ji(now)),
            kv("admin_copy_enabled", jb(n.admin_copy_enabled)),
            kv("admin_email", js(&n.admin_email)),
            kv("updated_at", ji(n.updated_at)),
        ];
        if let Some(ref ph) = n.admin_phone {
            v.push(kv("admin_phone", js(ph)));
        }
        s.wc("notification_settings", 1, &obj(v));
    }
    if let Some(ref lb) = d.landsbankinn {
        s.set_seq("landsbankinn_gateway_settings", 1);
        s.wc(
            "landsbankinn_gateway_settings",
            1,
            &obj(vec![
                kv("id", ji(1)),
                kv("created", ji(now)),
                kv("api_base_url", js(&lb.api_base_url)),
                kv("user_id", js(&lb.user_id)),
                kv("api_key", js(&lb.api_key)),
                kv("entity_id", js(&lb.entity_id)),
                kv("payment_contract_id", js(&lb.payment_contract_id)),
                kv("interaction_type", js(&lb.interaction_type)),
                kv("updated_at", ji(lb.updated_at)),
            ]),
        );
    }
    if let Some(ref pd) = d.payday {
        s.set_seq("payday_settings", 1);
        s.wc(
            "payday_settings",
            1,
            &obj(vec![
                kv("id", ji(1)),
                kv("created", ji(now)),
                kv("enabled", jb(pd.enabled)),
                kv("api_base_url", js(&pd.api_base_url)),
                kv("client_id", js(&pd.client_id)),
                kv("client_secret", js(&pd.client_secret)),
                kv("default_vat_percentage", ji(pd.default_vat_percentage)),
                kv(
                    "create_electronic_invoice",
                    jb(pd.create_electronic_invoice),
                ),
                kv("send_email", jb(pd.send_email)),
                kv("create_claim", jb(pd.create_claim)),
                kv("due_days", ji(pd.due_days)),
                kv("final_due_days", ji(pd.final_due_days)),
                kv("updated_at", ji(pd.updated_at)),
            ]),
        );
    }

    // users (coll)
    for (i, u) in d.users.iter().enumerate() {
        let id = (i + 1) as u64;
        s.set_seq("users", id);
        let mut v = vec![
            kv("id", ji(id as i64)),
            kv("created", ji(now)),
            kv("name", js(&u.name)),
            kv("role", js(&u.role)),
            kv("created_at", ji(u.created_at)),
            kv("updated_at", ji(u.updated_at)),
        ];
        if let Some(ref e) = u.email {
            v.push(kv("email", js(e)));
        }
        if let Some(ref p) = u.phone {
            v.push(kv("phone", js(p)));
        }
        if let Some(fp) = u.fixed_price {
            v.push(kv("fixed_price", ji(fp)));
        }
        if let Some(ref k) = u.kennitala {
            v.push(kv("kennitala", js(k)));
        }
        if let Some(ref p) = u.payday_customer_id {
            v.push(kv("payday_customer_id", js(p)));
        }
        s.wc("users", id, &obj(v));
    }

    // Auth users
    for au in &d.auth_users {
        let n = au.email.trim().to_ascii_lowercase();
        let role = d
            .users
            .iter()
            .find(|u| {
                u.email
                    .as_ref()
                    .map(|e| e.to_lowercase() == n)
                    .unwrap_or(false)
            })
            .map(|u| u.role.as_str())
            .unwrap_or("user");
        s.au.insert(
            n.as_bytes(),
            obj(vec![kv("email", js(&n)), kv("role", js(role))])
                .to_json()
                .as_bytes(),
        )
        .unwrap();
    }

    // Auth sessions
    for sess in &d.auth_sessions {
        s.as_
            .insert(
                sess.token.as_bytes(),
                obj(vec![
                    kv("email", js(&sess.email)),
                    kv("created", ji(sess.created_sec)),
                    kv("expires_at", ji(sess.expires_sec)),
                ])
                .to_json()
                .as_bytes(),
            )
            .unwrap();
    }

    // Booking users
    for bu in &d.booking_users {
        s.bu.insert(
            bu.id.as_bytes(),
            obj(vec![
                kv("id", js(&bu.id)),
                kv("fixed_price", joi(bu.fixed_price)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Bookings
    for bk in &d.bookings {
        s.bb.insert(
            &bk.starts_at.to_be_bytes(),
            obj(vec![
                kv("id", js(&bk.id)),
                kv("user_id", js(&bk.user_id)),
                kv("starts_at", ji(bk.starts_at)),
                kv("status", js(&bk.status)),
                kv("payment_type", js(&bk.payment_type)),
                kv("price_paid", joi(bk.price_paid)),
                kv("user_package_id", jos(&bk.user_package_id)),
                kv("user_subscription_id", jos(&bk.user_subscription_id)),
                kv("notes", jos(&bk.notes)),
                kv("created_at", ji(bk.created_at)),
                kv("cancelled_at", joi(bk.cancelled_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // User packages
    for up in &d.user_packages {
        s.bp.insert(
            up.id.as_bytes(),
            obj(vec![
                kv("id", js(&up.id)),
                kv("user_id", js(&up.user_id)),
                kv("remaining", ji(up.remaining)),
                kv("package_name", js(&up.package_name)),
                kv("slot_count", ji(up.slot_count)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // User subscriptions
    for us in &d.user_subs {
        let dl = d.sub_info.get(&us.sub_id).map(|s| s.0).unwrap_or(2);
        let vf = parse_date_ms(&us.valid_from);
        let vu = parse_date_ms(&us.valid_until);
        s.bs.insert(
            us.id.as_bytes(),
            obj(vec![
                kv("id", js(&us.id)),
                kv("user_id", js(&us.user_id)),
                kv("valid_from", ji(vf)),
                kv("valid_until", ji(vu)),
                kv("daily_limit", ji(dl)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Subscription members
    for sm in &d.sub_members {
        let key = format!("{}:{}", sm.user_subscription_id, sm.id);
        s.bm.insert(
            key.as_bytes(),
            obj(vec![
                kv("id", js(&sm.id)),
                kv("user_subscription_id", js(&sm.user_subscription_id)),
                kv("user_id", jos(&sm.user_id)),
                kv("role", js(&sm.role)),
                kv("status", js(&sm.status)),
                kv("invited_phone", jos(&sm.invited_phone)),
                kv("invited_at", ji(sm.invited_at)),
                kv("accepted_at", joi(sm.accepted_at)),
                kv("removed_at", joi(sm.removed_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Carts
    for ct in &d.carts {
        s.cc.insert(
            ct.id.as_bytes(),
            obj(vec![
                kv("id", js(&ct.id)),
                kv("user_id", jos(&ct.user_id)),
                kv("status", js(&ct.status)),
                kv("currency", js(&ct.currency)),
                kv("created_at", ji(ct.created_at)),
                kv("updated_at", ji(ct.updated_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Cart items
    for ci in &d.cart_items {
        let meta = ci
            .metadata
            .as_deref()
            .and_then(|r| akurai_json::parse(r).ok())
            .unwrap_or(Value::Null);
        s.ci.insert(
            ci.id.as_bytes(),
            obj(vec![
                kv("id", js(&ci.id)),
                kv("cart_id", js(&ci.cart_id)),
                kv("type", js(&ci.itype)),
                kv("ref_id", js(&ci.ref_id)),
                kv("name_snapshot", js(&ci.name_snapshot)),
                kv("unit_price", ji(ci.unit_price)),
                kv("quantity", ji(ci.quantity)),
                kv("metadata", meta),
                kv("created_at", ji(ci.created_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Gift cards (B+tree)
    for gc in &d.gift_cards {
        s.gc.insert(
            gc.id.as_bytes(),
            obj(vec![
                kv("id", js(&gc.id)),
                kv("code", js(&gc.code)),
                kv("amount", ji(gc.amount)),
                kv("balance", ji(gc.balance)),
                kv("currency", js(&gc.currency)),
                kv("status", js(&gc.status)),
                kv("expires_at", joi(gc.expires_at)),
                kv("delivery_at", joi(gc.delivery_at)),
                kv("delivered_at", joi(gc.delivered_at)),
                kv("purchased_by_user_id", jos(&gc.purchased_by_user_id)),
                kv("cart_id", jos(&gc.cart_id)),
                kv("recipient_email", jos(&gc.recipient_email)),
                kv("recipient_phone", jos(&gc.recipient_phone)),
                kv("recipient_name", jos(&gc.recipient_name)),
                kv("message", jos(&gc.message)),
                kv("created_at", ji(gc.created_at)),
                kv("updated_at", ji(gc.updated_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Gift card redemptions
    for gr in &d.gc_redemptions {
        s.gr.insert(
            gr.id.as_bytes(),
            obj(vec![
                kv("id", js(&gr.id)),
                kv("gift_card_id", js(&gr.gift_card_id)),
                kv("cart_id", jos(&gr.cart_id)),
                kv("payment_id", jos(&gr.payment_id)),
                kv("amount", ji(gr.amount)),
                kv("redeemed_at", ji(gr.redeemed_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Payments
    for pm in &d.payments {
        let raw = pm
            .raw_payload
            .as_deref()
            .and_then(|r| akurai_json::parse(r).ok())
            .unwrap_or(Value::Null);
        s.cp.insert(
            pm.id.as_bytes(),
            obj(vec![
                kv("id", js(&pm.id)),
                kv("cart_id", jos(&pm.cart_id)),
                kv("user_id", jos(&pm.user_id)),
                kv("provider", js(&pm.provider)),
                kv("provider_ref", js(&pm.provider_ref)),
                kv("status", js(&pm.status)),
                kv("amount", ji(pm.amount)),
                kv("currency", js(&pm.currency)),
                kv("raw_payload", raw),
                kv("created_at", ji(pm.created_at)),
                kv("updated_at", ji(pm.updated_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Checkout gift cards (keyed by code)
    for gc in &d.gift_cards {
        s.cg.insert(
            gc.code.as_bytes(),
            obj(vec![
                kv("code", js(&gc.code)),
                kv("amount", ji(gc.amount)),
                kv("balance", ji(gc.balance)),
                kv("currency", js(&gc.currency)),
                kv("status", js(&gc.status)),
                kv("recipient_email", jos(&gc.recipient_email)),
                kv("recipient_phone", jos(&gc.recipient_phone)),
                kv("recipient_name", jos(&gc.recipient_name)),
                kv("message", jos(&gc.message)),
                kv("purchased_by_user_id", jos(&gc.purchased_by_user_id)),
                kv("cart_id", jos(&gc.cart_id)),
                kv("created_at", ji(gc.created_at)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }

    // Admin settings
    if let Some(ref bt) = d.bank_transfer {
        s.ad.insert(
            b"bank_transfer",
            obj(vec![
                kv("bankName", js(&bt.bank_name)),
                kv("accountHolder", js(&bt.account_holder)),
                kv("kennitala", js(&bt.kennitala)),
                kv("accountNumber", js(&bt.account_number)),
            ])
            .to_json()
            .as_bytes(),
        )
        .unwrap();
    }
    if let Some(ref n) = d.notifications {
        let mut v = vec![
            kv("adminCopyEnabled", jb(n.admin_copy_enabled)),
            kv("adminEmail", js(&n.admin_email)),
        ];
        if let Some(ref ph) = n.admin_phone {
            v.push(kv("adminPhone", js(ph)));
        }
        s.ad.insert(b"notifications", obj(v).to_json().as_bytes())
            .unwrap();
    }
    for tpl in &d.sms_templates {
        s.ad.insert(
            format!("sms:{}", tpl.key).as_bytes(),
            obj(vec![kv("key", js(&tpl.key)), kv("body", js(&tpl.body))])
                .to_json()
                .as_bytes(),
        )
        .unwrap();
    }

    s.commit_all();

    println!(
        "  Wrote: {} cats, {} prods, {} pkgs, {} subs, {} prules",
        d.product_categories.len(),
        d.products.len(),
        d.packages.len(),
        d.subscriptions.len(),
        d.pricing_rules.len()
    );
    println!(
        "  Wrote: {} gc (coll), {} sms_tpl, {} email_tpl",
        d.gift_cards.len(),
        d.sms_templates.len(),
        d.email_templates.len()
    );
    println!(
        "  Wrote: settings (bt={} notif={} lb={} pd={})",
        d.bank_transfer.is_some(),
        d.notifications.is_some(),
        d.landsbankinn.is_some(),
        d.payday.is_some()
    );
    println!(
        "  Wrote: {} users (coll), {} auth, {} sessions",
        d.users.len(),
        d.auth_users.len(),
        d.auth_sessions.len()
    );
    println!(
        "  Wrote: {} bookings, {} bu, {} upkg, {} usub, {} smem",
        d.bookings.len(),
        d.booking_users.len(),
        d.user_packages.len(),
        d.user_subs.len(),
        d.sub_members.len()
    );
    println!(
        "  Wrote: {} carts, {} citems, {} gc (btree), {} gcr, {} pay, {} cg",
        d.carts.len(),
        d.cart_items.len(),
        d.gift_cards.len(),
        d.gc_redemptions.len(),
        d.payments.len(),
        d.gift_cards.len()
    );
}
