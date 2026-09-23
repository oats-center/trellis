use bytes::Bytes;
use futures_util::future::BoxFuture;

use super::request_loop::{HandlerResponse, RequestHandler};
use super::{
    AuthenticatedRouter, BootstrapBinding, RequestContext, RequestValidator, Router, ServerError,
};

/// A bootstrap-validated host wrapper for one service router.
pub struct ServiceHost<H> {
    service_name: String,
    binding: BootstrapBinding,
    handler: H,
}

impl<H> ServiceHost<H> {
    pub fn new(service_name: impl Into<String>, binding: BootstrapBinding, handler: H) -> Self {
        Self {
            service_name: service_name.into(),
            binding,
            handler,
        }
    }
}

impl<H> RequestHandler for ServiceHost<H>
where
    H: RequestHandler,
{
    fn handler_service_name(&self) -> Option<&str> {
        Some(&self.service_name)
    }

    fn handler_contract_id(&self) -> Option<&str> {
        Some(&self.binding.contract_id)
    }

    fn handler_contract_digest(&self) -> Option<&str> {
        Some(&self.binding.digest)
    }

    fn route_token(&self, subject: &str) -> Option<&'static str> {
        self.handler.route_token(subject)
    }

    fn is_unary_rpc_route(&self, subject: &str) -> bool {
        self.handler.is_unary_rpc_route(subject)
    }

    fn is_live_route(&self, subject: &str) -> bool {
        self.handler.is_live_route(subject)
    }

    fn handle<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
        self.handler.handle(subject, payload, context)
    }

    fn handle_frames<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Vec<Bytes>, ServerError>> {
        self.handler.handle_frames(subject, payload, context)
    }

    fn handle_response<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<HandlerResponse, ServerError>> {
        self.handler.handle_response(subject, payload, context)
    }
}

/// Bootstrap a service host from a previously resolved binding.
pub fn bootstrap_service_host<V>(
    service_name: &str,
    binding: BootstrapBinding,
    router: Router,
    validator: V,
) -> ServiceHost<AuthenticatedRouter<V>>
where
    V: RequestValidator + 'static,
{
    let authenticated_router = AuthenticatedRouter::new(router, validator);
    ServiceHost::new(service_name.to_string(), binding, authenticated_router)
}
