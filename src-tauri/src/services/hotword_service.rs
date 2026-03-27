use crate::asr::volc_auth::VolcAuth;
use crate::commands::settings::AppSettings;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

pub struct HotwordService {
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct VolcResponse<T> {
    #[serde(rename = "ResponseMetadata")]
    metadata: ResponseMetadata,
    #[serde(rename = "Result")]
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ResponseMetadata {
    #[serde(rename = "RequestId")]
    _request_id: String,
    #[serde(rename = "Error")]
    error: Option<VolcError>,
}

#[derive(Debug, Deserialize)]
struct VolcError {
    #[serde(rename = "Code")]
    code: String,
    #[serde(rename = "Message")]
    message: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BoostingTable {
    #[serde(rename = "AppID")]
    pub app_id: String,
    #[serde(rename = "BoostingTableID")]
    pub id: String,
    #[serde(rename = "BoostingTableName")]
    pub name: String,
    #[serde(rename = "CreateTime")]
    pub create_time: String,
    #[serde(rename = "UpdateTime")]
    pub update_time: String,
    #[serde(rename = "WordCount")]
    pub word_count: i32,
    #[serde(rename = "File")]
    pub file_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListBoostingTableResult {
    #[serde(rename = "BoostingTables")]
    tables: Vec<BoostingTable>,
}

impl HotwordService {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub async fn fetch_remote_tables(
        &self,
        settings: &AppSettings,
    ) -> Result<Vec<BoostingTable>, String> {
        let auth = VolcAuth::new(
            settings.volc_access_key.clone(),
            settings.volc_secret_key.clone(),
            "cn-north-1",
            "speech_saas_prod",
        );

        let query = HashMap::from([
            ("Action".to_string(), "ListBoostingTable".to_string()),
            ("Version".to_string(), "2022-08-30".to_string()),
        ]);

        let payload = json!({
            "Action": "ListBoostingTable",
            "Version": "2022-08-30",
            "AppID": settings.volc_app_id.parse::<i64>().unwrap_or(0),
            "PageNumber": 1,
            "PageSize": 100,
            "PreviewSize": 10,
        });
        let payload_str = serde_json::to_string(&payload).unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        );
        headers.insert("Host".to_string(), "open.volcengineapi.com".to_string());

        let auth_header = auth.sign("POST", "/", &query, &mut headers, payload_str.as_bytes())?;

        // Python's behavior: x-content-sha256 is NOT sent in headers, and Host is handled by requests
        headers.remove("Host");
        headers.remove("x-content-sha256");

        let mut req_headers = HeaderMap::new();
        for (k, v) in headers {
            req_headers.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        req_headers.insert(
            "Authorization",
            HeaderValue::from_str(&auth_header).unwrap(),
        );

        let url =
            format!("https://open.volcengineapi.com/?Action=ListBoostingTable&Version=2022-08-30");

        let response = self
            .client
            .post(url)
            .headers(req_headers)
            .body(payload_str)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let res: VolcResponse<ListBoostingTableResult> =
            response.json().await.map_err(|e| e.to_string())?;

        if let Some(err) = res.metadata.error {
            return Err(format!("{}: {}", err.code, err.message));
        }

        Ok(res.result.map(|r| r.tables).unwrap_or_default())
    }

    pub async fn get_table_detail(
        &self,
        settings: &AppSettings,
        table_id: &str,
    ) -> Result<BoostingTable, String> {
        let auth = VolcAuth::new(
            settings.volc_access_key.clone(),
            settings.volc_secret_key.clone(),
            "cn-north-1",
            "speech_saas_prod",
        );

        let query = HashMap::from([
            ("Action".to_string(), "GetBoostingTable".to_string()),
            ("Version".to_string(), "2022-08-30".to_string()),
        ]);

        let payload = json!({
            "Action": "GetBoostingTable",
            "Version": "2022-08-30",
            "AppID": settings.volc_app_id.parse::<i64>().unwrap_or(0),
            "BoostingTableID": table_id,
        });
        let payload_str = serde_json::to_string(&payload).unwrap();

        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        );
        headers.insert("Host".to_string(), "open.volcengineapi.com".to_string());

        let auth_header = auth.sign("POST", "/", &query, &mut headers, payload_str.as_bytes())?;

        headers.remove("Host");
        headers.remove("x-content-sha256");

        let mut req_headers = HeaderMap::new();
        for (k, v) in headers {
            req_headers.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        req_headers.insert(
            "Authorization",
            HeaderValue::from_str(&auth_header).unwrap(),
        );

        let url =
            format!("https://open.volcengineapi.com/?Action=GetBoostingTable&Version=2022-08-30");

        let response = self
            .client
            .post(url)
            .headers(req_headers)
            .body(payload_str)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        let root: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            format!(
                "Failed to parse GetBoostingTable response (status={}, bytes={}): {}",
                status,
                body.len(),
                e
            )
        })?;

        if let Some(err) = root.get("ResponseMetadata").and_then(|m| m.get("Error")) {
            let code = err
                .get("Code")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let message = err
                .get("Message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            return Err(format!("{}: {}", code, message));
        }

        let result = root
            .get("Result")
            .ok_or_else(|| "No result in response".to_string())?;

        let mut table: BoostingTable = if let Some(table_value) = result.get("BoostingTable") {
            serde_json::from_value(table_value.clone()).map_err(|e| e.to_string())?
        } else {
            serde_json::from_value(result.clone()).map_err(|e| e.to_string())?
        };

        if table.file_content.is_none() {
            if let Some(file) = result.get("File").and_then(|v| v.as_str()) {
                table.file_content = Some(file.to_string());
            } else if let Some(file) = result
                .get("BoostingTable")
                .and_then(|v| v.get("File"))
                .and_then(|v| v.as_str())
            {
                table.file_content = Some(file.to_string());
            }
        }

        if table.file_content.is_none() {
            let has_file_key = result.get("File").is_some()
                || result
                    .get("BoostingTable")
                    .and_then(|v| v.get("File"))
                    .is_some();
            let key_list = result
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            log::warn!(
                "GetBoostingTable missing File content (table_id={}, has_file_key={}, result_keys={:?})",
                table_id,
                has_file_key,
                key_list
            );
        }

        Ok(table)
    }

    pub async fn create_table(
        &self,
        settings: &AppSettings,
        name: &str,
        content: &str,
    ) -> Result<BoostingTable, String> {
        let auth = VolcAuth::new(
            settings.volc_access_key.clone(),
            settings.volc_secret_key.clone(),
            "cn-north-1",
            "speech_saas_prod",
        );

        let normalized = self.get_normalized_content(content);
        log::debug!(
            "Creating table '{}' with {} entries",
            name,
            normalized.lines().count()
        );

        // Since I'm under pressure to deliver, I'll use a helper to perform the signed multipart request.
        self.signed_multipart_post(
            &auth,
            "CreateBoostingTable",
            "2022-08-30",
            settings,
            Some(name),
            None,
            &normalized,
        )
        .await
    }

    pub async fn update_table(
        &self,
        settings: &AppSettings,
        table_id: &str,
        content: &str,
    ) -> Result<BoostingTable, String> {
        let auth = VolcAuth::new(
            settings.volc_access_key.clone(),
            settings.volc_secret_key.clone(),
            "cn-north-1",
            "speech_saas_prod",
        );

        let normalized = self.get_normalized_content(content);
        log::debug!(
            "Updating table ID '{}' with {} entries",
            table_id,
            normalized.lines().count()
        );

        self.signed_multipart_post(
            &auth,
            "UpdateBoostingTable",
            "2022-08-30",
            settings,
            None,
            Some(table_id),
            &normalized,
        )
        .await
    }

    pub fn get_normalized_content(&self, content: &str) -> String {
        let raw_lines: Vec<&str> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        let mut seen = std::collections::HashSet::new();
        let mut unique_lines = Vec::new();
        let mut duplicates = Vec::new();

        for line in raw_lines {
            if seen.contains(line) {
                duplicates.push(line);
            } else {
                seen.insert(line);
                unique_lines.push(line);
            }
        }

        log::debug!(
            "Normalizing hotwords: {} entry/entries -> {} unique ({} duplicates found)",
            unique_lines.len() + duplicates.len(),
            unique_lines.len(),
            duplicates.len()
        );

        if !duplicates.is_empty() {
            log::warn!("Duplicates found in hotword list: {:?}", duplicates);
        }

        if unique_lines.is_empty() {
            String::new()
        } else {
            // Join with Unix newlines only.
            // DO NOT push a trailing newline, as the server treats it as an empty line ('空行') and fails validation.
            unique_lines.join("\n")
        }
    }

    async fn signed_multipart_post(
        &self,
        auth: &VolcAuth,
        action: &str,
        version: &str,
        settings: &AppSettings,
        name: Option<&str>,
        table_id: Option<&str>,
        content: &str,
    ) -> Result<BoostingTable, String> {
        let query = HashMap::from([
            ("Action".to_string(), action.to_string()),
            ("Version".to_string(), version.to_string()),
        ]);

        let boundary = "---------------------------voicexboundary";
        let mut body = Vec::new();

        let mut fields = vec![
            ("Action", action),
            ("Version", version),
            ("AppID", &settings.volc_app_id),
        ];

        if let Some(n) = name {
            fields.push(("BoostingTableName", n));
        }

        for (k, v) in fields {
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", k).as_bytes(),
            );
            body.extend_from_slice(format!("{}\r\n", v).as_bytes());
        }

        if let Some(id) = table_id {
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"BoostingTableID\"\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(format!("{}\r\n", id).as_bytes());
        }

        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"File\"; filename=\"hotwords.txt\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        body.extend_from_slice(content.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            format!("multipart/form-data; boundary={}", boundary),
        );
        headers.insert("Host".to_string(), "open.volcengineapi.com".to_string());

        let auth_header = auth.sign("POST", "/", &query, &mut headers, &body)?;

        // Match Python's behavior: x-content-sha256 is NOT sent in headers, and Host is handled by reqwest
        headers.remove("Host");
        headers.remove("x-content-sha256");

        let mut req_headers = HeaderMap::new();
        for (k, v) in headers {
            req_headers.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        req_headers.insert(
            "Authorization",
            HeaderValue::from_str(&auth_header).unwrap(),
        );

        let url = format!(
            "https://open.volcengineapi.com/?Action={}&Version={}",
            action, version
        );

        let response = self
            .client
            .post(url)
            .headers(req_headers)
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let res: VolcResponse<BoostingTable> = response.json().await.map_err(|e| e.to_string())?;

        if let Some(err) = res.metadata.error {
            return Err(format!("{}: {}", err.code, err.message));
        }

        if let Some(ref r) = res.result {
            log::debug!(
                "Sync Complete: Server WordCount={}, ID={}",
                r.word_count,
                r.id
            );
        }

        res.result
            .ok_or_else(|| "No result in response".to_string())
    }
}
