use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

type HmacSha256 = Hmac<Sha256>;

pub struct VolcAuth {
    access_key: String,
    secret_key: String,
    region: String,
    service: String,
}

impl VolcAuth {
    pub fn new(access_key: String, secret_key: String, region: &str, service: &str) -> Self {
        Self {
            access_key,
            secret_key,
            region: region.to_string(),
            service: service.to_string(),
        }
    }

    pub fn sign(
        &self,
        method: &str,
        uri: &str,
        query: &HashMap<String, String>,
        headers: &mut HashMap<String, String>,
        payload: &[u8],
    ) -> Result<String, String> {
        let now = Utc::now();
        let x_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = now.format("%Y%m%d").to_string();

        headers.insert("x-date".to_string(), x_date.clone());

        let payload_hash = hex::encode(Sha256::digest(payload));
        headers.insert("x-content-sha256".to_string(), payload_hash.clone());

        // 1. Canonical Request
        let mut sorted_query_keys: Vec<_> = query.keys().collect();
        sorted_query_keys.sort();
        let canonical_query_string = sorted_query_keys
            .iter()
            .map(|&k| {
                format!(
                    "{}={}",
                    urlencoding::encode(k),
                    urlencoding::encode(query.get(k).unwrap())
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        let mut sorted_header_keys: Vec<_> = headers.keys().map(|k| k.to_lowercase()).collect();
        sorted_header_keys.sort();

        let mut canonical_headers = String::new();
        for k in &sorted_header_keys {
            // Find the original key to get value
            let val = headers
                .iter()
                .find(|(ok, _)| ok.to_lowercase() == *k)
                .map(|(_, v)| v.trim())
                .unwrap_or("");
            // Match Python script behavior: x-content-sha256 value is empty in canonicalHeaders
            // And use no space after colon
            let val_for_canonical = if k == "x-content-sha256" { "" } else { val };
            canonical_headers.push_str(&format!("{}:{}\n", k, val_for_canonical));
        }

        let signed_headers = sorted_header_keys.join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method.to_uppercase(),
            uri,
            canonical_query_string,
            canonical_headers,
            signed_headers,
            payload_hash
        );

        let hash_canonical_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        // 2. String to Sign
        let credential_scope = format!("{}/{}/{}/request", date_short, self.region, self.service);
        let string_to_sign = format!(
            "HMAC-SHA256\n{}\n{}\n{}",
            x_date, credential_scope, hash_canonical_request
        );

        // 3. Signing Key
        let k_date = hmac_sha256(self.secret_key.as_bytes(), date_short.as_bytes());
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, self.service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"request");

        // 4. Signature
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, credential_scope, signed_headers, signature
        );

        Ok(authorization)
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}
