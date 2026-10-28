//! Private AWS state. The local SQLite connection is only a per-invocation projection.
use crate::{Result, message, now};
use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{SignableBody, SignableRequest, SigningSettings, sign},
    sign::v4,
};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::Arc,
    time::{Duration, SystemTime},
};

pub fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[derive(Clone)]
pub struct Aws {
    pub table: String,
    pub bucket: String,
    pub region: String,
}

impl Aws {
    pub fn env() -> Result<Arc<Self>> {
        let get = |name| std::env::var(name).map_err(|_| message("Missing cloud configuration."));
        Ok(Arc::new(Self {
            table: get("PROMPTLIST_TABLE")?,
            bucket: get("PROMPTLIST_BUCKET")?,
            region: get("AWS_REGION")?,
        }))
    }

    // Blocking persistence is deliberate: a successful write must precede any
    // external Spotify mutation. The short-lived thread owns its blocking client.
    fn request(
        &self,
        service: &str,
        method: &str,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>)> {
        let service = service.to_owned();
        let method = method.to_owned();
        let region = self.region.clone();
        std::thread::spawn(move || {
            let key = std::env::var("AWS_ACCESS_KEY_ID")
                .map_err(|_| message("Missing execution role."))?;
            let secret = std::env::var("AWS_SECRET_ACCESS_KEY")
                .map_err(|_| message("Missing execution role."))?;
            let credentials = Credentials::new(
                key,
                secret,
                std::env::var("AWS_SESSION_TOKEN").ok(),
                None,
                "lambda-role",
            );
            let identity = credentials.into();
            let mut settings = SigningSettings::default();
            if service == "s3" {
                settings.payload_checksum_kind =
                    aws_sigv4::http_request::PayloadChecksumKind::XAmzSha256;
                settings.percent_encoding_mode =
                    aws_sigv4::http_request::PercentEncodingMode::Single;
            }
            let params = v4::SigningParams::builder()
                .identity(&identity)
                .region(&region)
                .name(&service)
                .time(SystemTime::now())
                .settings(settings)
                .build()
                .map_err(|_| message("Cannot sign AWS request."))?
                .into();
            let request = SignableRequest::new(
                &method,
                &url,
                headers.iter().map(|(k, v)| (k.as_str(), v.as_str())),
                SignableBody::Bytes(&body),
            )
            .map_err(|_| message("Cannot sign AWS request."))?;
            let (instructions, _) = sign(request, &params)
                .map_err(|_| message("Cannot sign AWS request."))?
                .into_parts();
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(12))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| message("Cloud transport unavailable."))?;
            let mut request = client
                .request(
                    reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                    &url,
                )
                .body(body);
            for (key, value) in headers {
                request = request.header(key, value);
            }
            for (key, value) in instructions.headers() {
                request = request.header(key, value);
            }
            let response = request
                .send()
                .map_err(|_| message("Cloud storage temporarily unavailable."))?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            response
                .take(60_000_001)
                .read_to_end(&mut bytes)
                .map_err(|_| message("Cloud storage read failed."))?;
            if bytes.len() > 60_000_000 {
                return Err(message("Cloud object exceeds size limit."));
            }
            Ok((status, bytes))
        })
        .join()
        .map_err(|_| message("Cloud transport failed."))?
    }

    pub fn ddb(&self, operation: &str, body: Value) -> Result<Value> {
        let (status, bytes) = self.request(
            "dynamodb",
            "POST",
            format!("https://dynamodb.{}.amazonaws.com/", self.region),
            vec![
                ("content-type".into(), "application/x-amz-json-1.0".into()),
                (
                    "x-amz-target".into(),
                    format!("DynamoDB_20120810.{operation}"),
                ),
            ],
            serde_json::to_vec(&body)?,
        )?;
        if status != 200 {
            let code = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v["__type"]
                        .as_str()
                        .map(|s| s.rsplit('#').next().unwrap_or("Unknown").to_owned())
                })
                .unwrap_or_default();
            return Err(message(format!(
                "Cloud operation {operation} was not confirmed (HTTP {status}, {code})."
            )));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn get(&self, pk: &str, sk: &str) -> Result<Option<Value>> {
        let reply = self.ddb(
            "GetItem",
            json!({"TableName":self.table,"Key":key(pk,sk),"ConsistentRead":true}),
        )?;
        Ok(reply.get("Item").cloned())
    }
    pub fn put(&self, pk: &str, sk: &str, value: &Value, expires: Option<i64>) -> Result<()> {
        self.ddb(
            "PutItem",
            json!({"TableName":self.table,"Item":item(pk,sk,value,expires)?}),
        )?;
        Ok(())
    }
    pub fn delete(&self, pk: &str, sk: &str) -> Result<()> {
        self.ddb(
            "DeleteItem",
            json!({"TableName":self.table,"Key":key(pk,sk)}),
        )?;
        Ok(())
    }
    pub fn query(&self, pk: &str, prefix: &str) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let mut request = json!({"TableName":self.table,"KeyConditionExpression":"pk=:p AND begins_with(sk,:s)","ExpressionAttributeValues":{":p":{"S":pk},":s":{"S":prefix}},"ConsistentRead":true});
            if !cursor.is_null() {
                request["ExclusiveStartKey"] = cursor;
            }
            let reply = self.ddb("Query", request)?;
            if let Some(items) = reply["Items"].as_array() {
                rows.extend(items.iter().cloned());
            }
            if rows.len() > 250_000 {
                return Err(message("Account storage limit exceeded."));
            }
            cursor = reply
                .get("LastEvaluatedKey")
                .cloned()
                .unwrap_or(Value::Null);
            if cursor.is_null() {
                return Ok(rows);
            }
        }
    }
    pub fn s3(&self, method: &str, path: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
        if path.split('/').any(|p| p == "..")
            || !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c))
        {
            return Err(message("Invalid private object key."));
        }
        let (status, bytes) = self.request(
            "s3",
            method,
            format!(
                "https://{}.s3.{}.amazonaws.com/{path}",
                self.bucket, self.region
            ),
            vec![],
            bytes,
        )?;
        if !(200..300).contains(&status) {
            return Err(message(format!(
                "Private object {method} failed (HTTP {status})."
            )));
        }
        Ok(bytes)
    }
    pub fn payload(&self, item: &Value) -> Result<Value> {
        if let Some(key) = item["blob"]["S"].as_str() {
            let bytes = self.s3("GET", key, vec![])?;
            let mut out = Vec::new();
            GzDecoder::new(bytes.as_slice())
                .take(50_000_001)
                .read_to_end(&mut out)
                .map_err(|_| message("Invalid private object."))?;
            if out.len() > 50_000_000 {
                return Err(message("Private object exceeds size limit."));
            }
            Ok(serde_json::from_slice(&out)?)
        } else {
            decode(item)
        }
    }

    pub fn purge_account(&self, pk: &str) -> Result<()> {
        if !pk.starts_with("account#") || pk.len() != 72 {
            return Err(message("Invalid account deletion scope."));
        }
        for row in self.query(pk, "row#")? {
            if let Some(sk) = row["sk"]["S"].as_str() {
                self.delete(pk, sk)?;
            }
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Listing {
            #[serde(default)]
            contents: Vec<Object>,
            next_continuation_token: Option<String>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Object {
            key: String,
        }
        for prefix in [
            format!("{}/", pk.replace('#', "/")),
            format!("temporary/{}/", pk.replace('#', "/")),
        ] {
            let mut cursor: Option<String> = None;
            loop {
                let mut url = url::Url::parse(&format!(
                    "https://{}.s3.{}.amazonaws.com/",
                    self.bucket, self.region
                ))
                .unwrap();
                url.query_pairs_mut()
                    .append_pair("list-type", "2")
                    .append_pair("prefix", &prefix);
                if let Some(token) = &cursor {
                    url.query_pairs_mut()
                        .append_pair("continuation-token", token);
                }
                let (status, raw) = self.request("s3", "GET", url.to_string(), vec![], vec![])?;
                if status != 200 {
                    return Err(message("Private object deletion listing failed."));
                }
                let listing: Listing = quick_xml::de::from_reader(raw.as_slice())
                    .map_err(|_| message("Invalid private object listing."))?;
                for object in listing.contents {
                    if !object.key.starts_with(&prefix) {
                        return Err(message("Object deletion scope mismatch."));
                    }
                    self.s3("DELETE", &object.key, vec![])?;
                }
                cursor = listing.next_continuation_token;
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(())
    }
    pub fn lock(self: &Arc<Self>, pk: &str, seconds: i64) -> Result<Lease> {
        let owner = crate::provider::random_secret();
        let until = now() + seconds;
        self.ddb("PutItem",json!({"TableName":self.table,"Item":{"pk":{"S":pk},"sk":{"S":"lock"},"owner":{"S":owner},"expires":{"N":until.to_string()}},"ConditionExpression":"attribute_not_exists(pk) OR expires < :now","ExpressionAttributeValues":{":now":{"N":now().to_string()}}}))?;
        Ok(Lease {
            aws: self.clone(),
            pk: pk.into(),
            owner,
            until,
        })
    }
    pub fn admit(&self, name: &str, max: u32, expires: i64) -> Result<()> {
        self.ddb("UpdateItem",json!({"TableName":self.table,"Key":key("budget",name),"UpdateExpression":"SET expires=:e ADD used :one","ConditionExpression":"attribute_not_exists(used) OR used < :max","ExpressionAttributeValues":{":one":{"N":"1"},":max":{"N":max.to_string()},":e":{"N":expires.to_string()}}}))?;
        Ok(())
    }
}

pub fn key(pk: &str, sk: &str) -> Value {
    json!({"pk":{"S":pk},"sk":{"S":sk}})
}
pub fn item(pk: &str, sk: &str, payload: &Value, expires: Option<i64>) -> Result<Value> {
    let mut value = key(pk, sk);
    value["payload"] = json!({"S":serde_json::to_string(payload)?});
    if let Some(expires) = expires {
        value["expires"] = json!({"N":expires.to_string()});
    }
    Ok(value)
}
pub fn decode(item: &Value) -> Result<Value> {
    Ok(serde_json::from_str(
        item["payload"]["S"]
            .as_str()
            .ok_or_else(|| message("Invalid cloud record."))?,
    )?)
}

pub struct Lease {
    pub aws: Arc<Aws>,
    pub pk: String,
    pub owner: String,
    pub until: i64,
}
impl Lease {
    pub fn check(&self) -> Value {
        json!({"ConditionCheck":{"TableName":self.aws.table,"Key":key(&self.pk,"lock"),"ConditionExpression":"#o=:o AND expires>=:n","ExpressionAttributeNames":{"#o":"owner"},"ExpressionAttributeValues":{":o":{"S":self.owner},":n":{"N":now().to_string()}}}})
    }
    pub fn release(&self) -> Result<()> {
        self.aws.ddb("DeleteItem",json!({"TableName":self.aws.table,"Key":key(&self.pk,"lock"),"ConditionExpression":"#o=:o","ExpressionAttributeNames":{"#o":"owner"},"ExpressionAttributeValues":{":o":{"S":self.owner}}}))?;
        Ok(())
    }
}

pub struct Mirror {
    pub aws: Arc<Aws>,
    pub pk: String,
    pub lease: Option<Arc<Lease>>,
    rows: BTreeMap<String, Value>,
    objects: BTreeMap<String, String>,
}

impl Mirror {
    pub fn load(
        aws: Arc<Aws>,
        account: &str,
        lease: Option<Arc<Lease>>,
        db: &rusqlite::Connection,
    ) -> Result<Self> {
        let pk = format!("account#{}", hash(account));
        let mut rows = BTreeMap::new();
        let mut objects = BTreeMap::new();
        for item in aws.query(&pk, "row#")? {
            if item["expires"]["N"]
                .as_str()
                .and_then(|n| n.parse::<i64>().ok())
                .is_some_and(|n| n <= now())
            {
                continue;
            }
            let sk = item["sk"]["S"]
                .as_str()
                .ok_or_else(|| message("Invalid private row."))?
                .to_owned();
            if let Some(blob) = item["blob"]["S"].as_str() {
                objects.insert(sk.clone(), blob.into());
            }
            let row = aws.payload(&item)?;
            restore(db, &row, account)?;
            rows.insert(sk, row);
        }
        Ok(Self {
            aws,
            pk,
            lease,
            rows,
            objects,
        })
    }
    pub fn flush(&mut self, db: &rusqlite::Connection) -> Result<()> {
        let Some(lease) = &self.lease else {
            return Ok(());
        };
        if now() >= lease.until - 3 {
            return Err(message(
                "Cloud write lease expired; retry from saved state.",
            ));
        }
        let current = dump(db)?;
        let changed: Vec<_> = current
            .iter()
            .filter(|(k, v)| self.rows.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .chain(
                self.rows
                    .keys()
                    .filter(|k| !current.contains_key(*k))
                    .map(|k| (k.clone(), None)),
            )
            .collect();
        for chunk in changed.chunks(90) {
            let mut writes = vec![lease.check()];
            let mut uploads = BTreeMap::new();
            for (sk, row) in chunk {
                if let Some(row) = row {
                    let raw = serde_json::to_vec(row)?;
                    let expires = row_expiry(row);
                    let mut data = item(&self.pk, sk, row, expires)?;
                    if raw.len() > 120_000 {
                        let path = format!(
                            "{}{}/{}/{}.json.gz",
                            if expires.is_some() { "temporary/" } else { "" },
                            self.pk.replace('#', "/"),
                            hash(sk),
                            hash(std::str::from_utf8(&raw).unwrap())
                        );
                        let mut encoder = GzEncoder::new(vec![], Compression::fast());
                        encoder
                            .write_all(&raw)
                            .map_err(|_| message("Compression failed."))?;
                        self.aws.s3(
                            "PUT",
                            &path,
                            encoder
                                .finish()
                                .map_err(|_| message("Compression failed."))?,
                        )?;
                        data = key(&self.pk, sk);
                        data["blob"] = json!({"S":path});
                        if let Some(expires) = expires {
                            data["expires"] = json!({"N":expires.to_string()});
                        }
                        uploads.insert(sk.clone(), path);
                    }
                    writes.push(json!({"Put":{"TableName":self.aws.table,"Item":data}}));
                } else {
                    writes.push(
                        json!({"Delete":{"TableName":self.aws.table,"Key":key(&self.pk,sk)}}),
                    );
                }
            }
            self.aws
                .ddb("TransactWriteItems", json!({"TransactItems":writes}))?;
            for (sk, row) in chunk {
                if let Some(old) = self.objects.remove(sk) {
                    let _ = self.aws.s3("DELETE", &old, vec![]);
                }
                if let Some(path) = uploads.remove(sk) {
                    self.objects.insert(sk.clone(), path);
                }
                match row {
                    Some(row) => {
                        self.rows.insert(sk.clone(), row.clone());
                    }
                    None => {
                        self.rows.remove(sk);
                    }
                }
            }
        }
        Ok(())
    }
}

const TABLES: [(&str, usize); 6] = [
    ("records", 2),
    ("tag_cache", 2),
    ("audit_users", 1),
    ("prompt_runs", 2),
    ("app_events", 1),
    ("event_daily", 4),
];
fn row_expiry(row: &Value) -> Option<i64> {
    let v = &row["values"];
    match row["table"].as_str()? {
        "records" if v[1] != "snapshot" => v[3].as_i64().map(|n| n + 86400),
        "tag_cache" => v[3].as_i64(),
        "app_events" => v[2].as_i64().map(|n| n + 90 * 86400),
        "event_daily" => v[1].as_i64().map(|n| n + 365 * 86400),
        _ => None,
    }
}
pub fn dump(db: &rusqlite::Connection) -> Result<BTreeMap<String, Value>> {
    use rusqlite::types::ValueRef;
    let mut output = BTreeMap::new();
    for (table, keys) in TABLES {
        let mut stmt = db.prepare(&format!("SELECT * FROM {table}"))?;
        let count = stmt.column_count();
        let rows = stmt.query_map([], |row| {
            let mut values = Vec::new();
            for i in 0..count {
                values.push(match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Text(v) => Value::String(String::from_utf8_lossy(v).into_owned()),
                    _ => Value::Null,
                });
            }
            Ok(values)
        })?;
        for row in rows {
            let values = row?;
            let id = hash(&serde_json::to_string(&values[..keys])?);
            output.insert(
                format!("row#{table}#{id}"),
                json!({"table":table,"values":values}),
            );
        }
    }
    Ok(output)
}
fn restore(db: &rusqlite::Connection, row: &Value, account: &str) -> Result<()> {
    let table = row["table"]
        .as_str()
        .filter(|t| TABLES.iter().any(|(name, _)| name == t))
        .ok_or_else(|| message("Invalid private row type."))?;
    let values = row["values"]
        .as_array()
        .ok_or_else(|| message("Invalid private row."))?;
    let account_index = if table == "app_events" { 1 } else { 0 };
    if values.get(account_index).and_then(Value::as_str) != Some(account) {
        return Err(message("Private row account mismatch."));
    }
    let values: Vec<rusqlite::types::Value> = values
        .iter()
        .map(|v| match v {
            Value::Null => rusqlite::types::Value::Null,
            Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
            _ => rusqlite::types::Value::Null,
        })
        .collect();
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO {table} VALUES({})",
            vec!["?"; values.len()].join(",")
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

pub fn import_local(aws: Arc<Aws>, path: &std::path::Path) -> Result<(usize, usize)> {
    let source =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(Duration::from_secs(5))?;
    let mut copy = rusqlite::Connection::open_in_memory()?;
    rusqlite::backup::Backup::new(&source, &mut copy)?.run_to_completion(
        100,
        Duration::from_millis(10),
        None,
    )?;
    crate::history::init(&copy)?;
    let rows = dump(&copy)?;
    let accounts: std::collections::BTreeSet<String> = rows
        .values()
        .filter(|r| r["table"] == "records" && r["values"][1] == "snapshot")
        .filter_map(|r| r["values"][0].as_str().map(str::to_owned))
        .collect();
    let mut written = 0;
    for account in &accounts {
        let pk = format!("account#{}", hash(account));
        let lease = Arc::new(aws.lock(&pk, 650)?);
        if !aws.query(&pk, "row#")?.is_empty() {
            lease.release()?;
            return Err(message(
                "Cloud account is not empty. Refusing to overwrite saved cloud data.",
            ));
        }
        let generation = crate::provider::random_secret();
        aws.put(&pk, "control", &json!({"generation":generation}), None)?;
        let mut store = crate::store::Store::memory()?;
        store.attach_cloud(aws.clone(), account, Some(lease.clone()))?;
        for row in rows.values() {
            let index = if row["table"] == "app_events" { 1 } else { 0 };
            if row["values"][index] == *account && row_expiry(row).is_none_or(|v| v > now()) {
                restore(&store.db, row, account)?;
                written += 1;
            }
        }
        store.flush()?;
        lease.release()?;
    }
    Ok((accounts.len(), written))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_roundtrip_keeps_history_and_does_not_double_count_events() {
        let s = crate::store::Store::memory().unwrap();
        crate::history::ensure_user(&s.db, "owner").unwrap();
        crate::history::event(
            &s.db,
            "owner",
            "build",
            None,
            json!({"http_status":200,"request_ms":25}),
        )
        .unwrap();
        let rows = dump(&s.db).unwrap();
        let other = crate::store::Store::memory().unwrap();
        for row in rows.values() {
            restore(&other.db, row, "owner").unwrap();
        }
        assert_eq!(dump(&other.db).unwrap(), rows);
        for row in rows.values() {
            restore(&other.db, row, "owner").unwrap();
        }
        assert_eq!(dump(&other.db).unwrap(), rows);
    }
    #[test]
    fn projection_rejects_foreign_accounts_and_unknown_tables() {
        let s = crate::store::Store::memory().unwrap();
        assert!(
            restore(
                &s.db,
                &json!({"table":"records","values":["other","snapshot","{}",10]}),
                "owner"
            )
            .is_err()
        );
        assert!(
            restore(
                &s.db,
                &json!({"table":"records; DROP TABLE records","values":[]}),
                "owner"
            )
            .is_err()
        );
        assert!(dump(&s.db).unwrap().is_empty());
    }
    #[test]
    fn cloud_retention_matches_local_policy() {
        assert_eq!(
            row_expiry(&json!({"table":"records","values":["a","snapshot","{}",10]})),
            None
        );
        assert_eq!(
            row_expiry(&json!({"table":"records","values":["a","draft","{}",10]})),
            Some(86410)
        );
        assert_eq!(
            row_expiry(&json!({"table":"app_events","values":[1,"a",10,"view",null,"{}"]})),
            Some(10 + 90 * 86400)
        );
        assert_eq!(
            row_expiry(&json!({"table":"event_daily","values":["a",0,"view","ok",1,0,0]})),
            Some(365 * 86400)
        );
        assert_eq!(
            row_expiry(&json!({"table":"prompt_runs","values":["a","r",10,"{}"]})),
            None
        );
    }
    #[test]
    fn opaque_keys_are_stable_and_session_data_is_not_debug_serialized() {
        assert_eq!(hash("private-account").len(), 64);
        assert_eq!(hash("same"), hash("same"));
        assert_ne!(hash("a"), hash("b"));
        let row = item("opaque", "session", &json!({"csrf":"synthetic"}), Some(123)).unwrap();
        assert_eq!(decode(&row).unwrap()["csrf"], "synthetic");
        assert_eq!(row["expires"]["N"], "123");
    }
}
