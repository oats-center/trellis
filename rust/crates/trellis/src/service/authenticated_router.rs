use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{stream, StreamExt};
use std::sync::Arc;
use std::time::Duration;

use super::local_validator::VerifiedCaller;
use super::request_loop::{HandlerResponse, ResponseStream};
use super::{RequestContext, Router, ServerError};

/// Result returned by request validators after checking caller authorization.
#[derive(Debug, Clone, Default, PartialEq)]
#[doc = concat!("Public Trellis data type `", stringify!(RequestValidation), "`.")]
pub struct RequestValidation {
    #[doc = concat!("The `", stringify!(allowed), "` value.")]
    pub allowed: bool,
    #[doc = concat!("The `", stringify!(caller), "` value.")]
    pub caller: Option<VerifiedCaller>,
    /// Server-authorized reply inbox prefix for this session.
    pub inbox_prefix: Option<String>,
}

impl RequestValidation {
    /// Construct an allowed validation result with no caller metadata.
    #[doc = concat!("Trellis API operation `", stringify!(allowed), "`.")]
    pub fn allowed() -> Self {
        Self {
            allowed: true,
            caller: None,
            inbox_prefix: None,
        }
    }

    /// Construct an allowed validation result with caller metadata.
    #[doc = concat!("Trellis API operation `", stringify!(allowed_caller), "`.")]
    pub fn allowed_caller(caller: VerifiedCaller) -> Self {
        Self {
            allowed: true,
            caller: Some(caller),
            inbox_prefix: None,
        }
    }

    /// Construct a denied validation result.
    #[doc = concat!("Trellis API operation `", stringify!(denied), "`.")]
    pub fn denied() -> Self {
        Self {
            allowed: false,
            caller: None,
            inbox_prefix: None,
        }
    }
}

/// Auth validator called before dispatching requests to mounted handlers.
pub trait RequestValidator: Send + Sync {
    fn validate<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>>;

    /// Verify caller possession for a separately grant-authorized transport request.
    fn validate_possession<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        self.validate(subject, payload, context)
    }

    /// Recheck that an admitted streaming request remains authorized without
    /// replaying its one-shot request proof.
    fn revalidate_current<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>>;
}

impl<V> RequestValidator for std::sync::Arc<V>
where
    V: RequestValidator + ?Sized,
{
    fn validate<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        (**self).validate(subject, payload, context)
    }

    fn validate_possession<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Bytes,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
        (**self).validate_possession(subject, payload, context)
    }

    fn revalidate_current<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<bool, ServerError>> {
        (**self).revalidate_current(context)
    }
}

/// A router wrapper that enforces auth validation before handler execution.
pub struct AuthenticatedRouter<V>
where
    V: RequestValidator,
{
    router: Router,
    validator: Arc<V>,
}

impl<V> AuthenticatedRouter<V>
where
    V: RequestValidator + 'static,
{
    #[doc = concat!("Trellis API operation `", stringify!(new), "`.")]
    pub fn new(router: Router, validator: V) -> Self {
        Self {
            router,
            validator: Arc::new(validator),
        }
    }

    #[doc = concat!("Trellis API operation `", stringify!(inner), "`.")]
    pub fn inner(&self) -> &Router {
        &self.router
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(handle_request), "`.")]
    pub async fn handle_request(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<Bytes, ServerError> {
        let session_key = String::new();

        if context
            .proof
            .as_deref()
            .map(|proof| proof.is_empty())
            .unwrap_or(true)
        {
            return Err(ServerError::MissingProof {
                subject: subject.to_string(),
            });
        }

        let context = self.context_with_required_capabilities(subject, &payload, context)?;
        let validation = self.validator.validate(subject, &payload, &context).await?;
        if !validation.allowed {
            return Err(ServerError::RequestDenied {
                subject: subject.to_string(),
                session_key,
            });
        }

        validate_reply_inbox(
            subject,
            &session_key,
            validation.inbox_prefix.as_deref(),
            context.reply_to.as_deref(),
        )?;

        let context = RequestContext {
            caller: validation.caller,
            ..context
        };
        self.router.handle_request(subject, payload, context).await
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(handle_request_frames), "`.")]
    pub async fn handle_request_frames(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<Vec<Bytes>, ServerError> {
        let session_key = String::new();

        if context
            .proof
            .as_deref()
            .map(|proof| proof.is_empty())
            .unwrap_or(true)
        {
            return Err(ServerError::MissingProof {
                subject: subject.to_string(),
            });
        }

        let context = self.context_with_required_capabilities(subject, &payload, context)?;
        let validation = self.validator.validate(subject, &payload, &context).await?;
        if !validation.allowed {
            return Err(ServerError::RequestDenied {
                subject: subject.to_string(),
                session_key,
            });
        }

        validate_reply_inbox(
            subject,
            &session_key,
            validation.inbox_prefix.as_deref(),
            context.reply_to.as_deref(),
        )?;

        let context = RequestContext {
            caller: validation.caller,
            ..context
        };
        self.router
            .handle_request_frames(subject, payload, context)
            .await
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(handle_request_response), "`.")]
    pub async fn handle_request_response(
        &self,
        subject: &str,
        payload: Bytes,
        context: RequestContext,
    ) -> Result<HandlerResponse, ServerError> {
        let session_key = String::new();

        if context
            .proof
            .as_deref()
            .map(|proof| proof.is_empty())
            .unwrap_or(true)
        {
            return Err(ServerError::MissingProof {
                subject: subject.to_string(),
            });
        }

        let context = self.context_with_required_capabilities(subject, &payload, context)?;
        let validation = self.validator.validate(subject, &payload, &context).await?;
        if !validation.allowed {
            return Err(ServerError::RequestDenied {
                subject: subject.to_string(),
                session_key,
            });
        }

        validate_reply_inbox(
            subject,
            &session_key,
            validation.inbox_prefix.as_deref(),
            context.reply_to.as_deref(),
        )?;

        let context = RequestContext {
            caller: validation.caller,
            ..context
        };
        let stream_context = context.clone();
        let response = self
            .router
            .handle_request_response(subject, payload, context)
            .await?;
        Ok(match response {
            HandlerResponse::Stream(stream) => HandlerResponse::Stream(revalidating_stream(
                stream,
                Arc::clone(&self.validator),
                stream_context,
            )),
            response => response,
        })
    }

    fn context_with_required_capabilities(
        &self,
        subject: &str,
        payload: &[u8],
        context: RequestContext,
    ) -> Result<RequestContext, ServerError> {
        Ok(RequestContext {
            required_capabilities: self.router.required_capabilities(subject, payload)?,
            required_permission: self.router.required_permission(subject, payload)?,
            ..context
        })
    }
}

fn revalidating_stream<V: RequestValidator + 'static>(
    stream: ResponseStream,
    validator: Arc<V>,
    context: RequestContext,
) -> ResponseStream {
    Box::pin(stream::unfold(
        (stream, validator, context),
        |(mut frames, validator, context)| async move {
            loop {
                tokio::select! {
                    frame = frames.next() => return frame.map(|frame| (frame, (frames, validator, context))),
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        match validator.revalidate_current(&context).await {
                            Ok(true) => {}
                            Ok(false) => return None,
                            Err(error) => return Some((Err(error), (frames, validator, context))),
                        }
                    }
                }
            }
        },
    ))
}

fn validate_reply_inbox(
    subject: &str,
    session_key: &str,
    authorized_prefix: Option<&str>,
    reply_to: Option<&str>,
) -> Result<(), ServerError> {
    let Some(reply_to) = reply_to else {
        return Ok(());
    };
    let fallback = format!("_INBOX.{}", &session_key[..16.min(session_key.len())]);
    let prefix = authorized_prefix.unwrap_or(&fallback);
    if reply_to == prefix || reply_to.starts_with(&format!("{prefix}.")) {
        return Ok(());
    }

    tracing::warn!(reply_to, prefix, "request reply inbox prefix mismatch");
    Err(ServerError::ReplyInboxMismatch {
        subject: subject.to_string(),
        session_key: session_key.to_string(),
        reply_to: reply_to.to_string(),
        expected_prefix: prefix.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct RevocableValidator(AtomicBool);

    impl RequestValidator for RevocableValidator {
        fn validate<'a>(
            &'a self,
            _subject: &'a str,
            _payload: &'a Bytes,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
            Box::pin(async { Ok(RequestValidation::allowed()) })
        }

        fn revalidate_current<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<bool, ServerError>> {
            Box::pin(async { Ok(self.0.load(Ordering::SeqCst)) })
        }
    }

    #[tokio::test]
    async fn admitted_stream_ends_after_current_authorization_is_revoked() {
        let validator = Arc::new(RevocableValidator(AtomicBool::new(false)));
        let mut stream = revalidating_stream(
            Box::pin(stream::pending()),
            validator,
            RequestContext::default(),
        );

        assert!(
            tokio::time::timeout(Duration::from_millis(1_500), stream.next())
                .await
                .expect("authorization recheck should finish")
                .is_none()
        );
    }
}
