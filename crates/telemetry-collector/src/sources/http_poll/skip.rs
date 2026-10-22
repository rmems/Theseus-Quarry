use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipClass {
    HttpSend,
    HttpStatus,
    WrongContentType,
    OversizeBody,
    InvalidJson,
    NotJsonObject,
}

impl SkipClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HttpSend => "http_send",
            Self::HttpStatus => "http_status",
            Self::WrongContentType => "wrong_content_type",
            Self::OversizeBody => "oversize_body",
            Self::InvalidJson => "invalid_json",
            Self::NotJsonObject => "not_json_object",
        }
    }
}

fn status_class_from_u16(code: u16) -> &'static str {
    match code {
        200..=299 => "success",
        400..=499 => "client_error",
        500..=599 => "server_error",
        _ => "other",
    }
}

pub(crate) fn log_skip(adapter: &str, class: SkipClass, http_status: Option<u16>) {
    if let Some(code) = http_status {
        warn!(
            adapter,
            skip = class.as_str(),
            http_status = code,
            status_class = status_class_from_u16(code),
            "HTTP poll skipped"
        );
    } else {
        warn!(adapter, skip = class.as_str(), "HTTP poll skipped");
    }
}
