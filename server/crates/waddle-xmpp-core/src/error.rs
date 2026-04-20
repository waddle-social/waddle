use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    #[error("bad request")]
    BadRequest(Option<String>),

    #[error("not acceptable")]
    NotAcceptable(Option<String>),

    #[error("waddle-xmpp-core scaffold is not implemented yet")]
    NotImplemented,
}

impl CoreError {
    pub fn bad_request(text: Option<String>) -> Self {
        Self::BadRequest(text)
    }

    pub fn not_acceptable(text: Option<String>) -> Self {
        Self::NotAcceptable(text)
    }
}
