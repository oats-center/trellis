//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "trellis.console";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Browser;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[
        "trellis.auth@v1::accountRead",
        "trellis.auth@v1::identitiesManage",
        "trellis.auth@v1::identitiesRead",
        "trellis.auth@v1::passwordChange",
        "trellis.auth@v1::sessionsRead",
        "trellis.auth@v1::sessionsRevoke",
        "trellis.core@v1::resources_destroy",
        "trellis.core@v1::resources_read",
        "trellis.events@v1::consumersInspect",
        "trellis.events@v1::deadLettersDismiss",
        "trellis.events@v1::deadLettersInspect",
        "trellis.events@v1::deadLettersReplay",
        "trellis.events@v1::diagnostics",
        "trellis.events@v1::inspect",
        "trellis.events@v1::metrics",
        "trellis.events@v1::observe",
        "trellis.events@v1::query",
        "trellis.health@v1::inspect",
        "trellis.health@v1::metrics",
        "trellis.health@v1::observe",
        "trellis.health@v1::query",
        "trellis.health@v1::summary",
        "trellis.jobs@v1::cancel",
        "trellis.jobs@v1::dlqDismiss",
        "trellis.jobs@v1::dlqRead",
        "trellis.jobs@v1::dlqReplay",
        "trellis.jobs@v1::inspect",
        "trellis.jobs@v1::keyLookup",
        "trellis.jobs@v1::listServices",
        "trellis.jobs@v1::metrics",
        "trellis.jobs@v1::observe",
        "trellis.jobs@v1::query",
        "trellis.jobs@v1::retry",
        "trellis.jobs@v1::summary",
        "trellis.state@v1::delete",
        "trellis.state@v1::read",
        "trellis.state@v1::resourcesInspect",
        "trellis.state@v1::write",
    ];
    const OPTIONAL_CAPABILITIES: &'static [&'static str] = &[
        "trellis.auth@v1::apisAccept",
        "trellis.auth@v1::apisForceReplace",
        "trellis.auth@v1::apisRead",
        "trellis.auth@v1::apisReview",
        "trellis.auth@v1::authorizationSessionsRead",
        "trellis.auth@v1::authorizationSessionsRevoke",
        "trellis.auth@v1::capabilitiesRead",
        "trellis.auth@v1::capabilityGrantsGrant",
        "trellis.auth@v1::capabilityGrantsRead",
        "trellis.auth@v1::capabilityGrantsRevoke",
        "trellis.auth@v1::connectionsKick",
        "trellis.auth@v1::connectionsRead",
        "trellis.auth@v1::delegationsRead",
        "trellis.auth@v1::delegationsRevoke",
        "trellis.auth@v1::deploymentsApply",
        "trellis.auth@v1::deploymentsCreate",
        "trellis.auth@v1::deploymentsDisable",
        "trellis.auth@v1::deploymentsEnable",
        "trellis.auth@v1::deploymentsRead",
        "trellis.auth@v1::deploymentsRemove",
        "trellis.auth@v1::devicesDisable",
        "trellis.auth@v1::devicesEnable",
        "trellis.auth@v1::devicesProvision",
        "trellis.auth@v1::devicesRead",
        "trellis.auth@v1::devicesRemove",
        "trellis.auth@v1::issuersRevoke",
        "trellis.auth@v1::issuersRotate",
        "trellis.auth@v1::oauthClientsDelete",
        "trellis.auth@v1::oauthClientsDisable",
        "trellis.auth@v1::oauthClientsPut",
        "trellis.auth@v1::oauthClientsRead",
        "trellis.auth@v1::oidcProvidersDelete",
        "trellis.auth@v1::oidcProvidersDisable",
        "trellis.auth@v1::oidcProvidersPut",
        "trellis.auth@v1::oidcProvidersRead",
        "trellis.auth@v1::oidcRoleMappingsDelete",
        "trellis.auth@v1::oidcRoleMappingsPut",
        "trellis.auth@v1::oidcRoleMappingsRead",
        "trellis.auth@v1::platformDelegationsPut",
        "trellis.auth@v1::platformDelegationsRead",
        "trellis.auth@v1::platformDelegationsRevoke",
        "trellis.auth@v1::platformPrivilegesAssign",
        "trellis.auth@v1::platformPrivilegesRead",
        "trellis.auth@v1::platformPrivilegesRevoke",
        "trellis.auth@v1::principalsEffectiveAccess",
        "trellis.auth@v1::resourcesRead",
        "trellis.auth@v1::resourcesRemove",
        "trellis.auth@v1::roleAssignmentsAssign",
        "trellis.auth@v1::roleAssignmentsRead",
        "trellis.auth@v1::roleAssignmentsRevoke",
        "trellis.auth@v1::rolesDelete",
        "trellis.auth@v1::rolesPut",
        "trellis.auth@v1::rolesRead",
        "trellis.auth@v1::securityObserve",
        "trellis.auth@v1::serviceInstancesDisable",
        "trellis.auth@v1::serviceInstancesEnable",
        "trellis.auth@v1::serviceInstancesProvision",
        "trellis.auth@v1::serviceInstancesRead",
        "trellis.auth@v1::serviceInstancesRemove",
        "trellis.auth@v1::usersCreate",
        "trellis.auth@v1::usersPasswordReset",
        "trellis.auth@v1::usersRead",
        "trellis.auth@v1::usersUpdate",
    ];
}
const OPTIONAL_ACTIONS: &[trellis_rs::generated::OptionalAction] = &[
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Apis.Accept"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Apis.ForceReplace"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Apis.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Apis.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Apis.Review"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "AuthorizationSessions.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "AuthorizationSessions.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Capabilities.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "CapabilityGrants.Grant"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "CapabilityGrants.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "CapabilityGrants.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Connections.Kick"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Connections.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Delegations.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Delegations.Review"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Delegations.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Apply"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Create"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Disable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Enable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Deployments.Remove"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Devices.Disable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Devices.Enable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Devices.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Devices.Provision"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Devices.Remove"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Issuers.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Issuers.Rotate"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OAuthClients.Delete"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OAuthClients.Disable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OAuthClients.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OAuthClients.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OAuthClients.Put"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCProviders.Delete"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCProviders.Disable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCProviders.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCProviders.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCProviders.Put"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCRoleMappings.Delete"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCRoleMappings.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "OIDCRoleMappings.Put"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformDelegations.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformDelegations.Put"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformDelegations.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformPrivileges.Assign"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformPrivileges.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "PlatformPrivileges.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Principals.EffectiveAccess"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Resources.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Resources.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Resources.Remove"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "RoleAssignments.Assign"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "RoleAssignments.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "RoleAssignments.Revoke"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Roles.Delete"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Roles.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Roles.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Roles.Put"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "ServiceInstances.Disable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "ServiceInstances.Enable"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "ServiceInstances.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "ServiceInstances.Provision"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "ServiceInstances.Remove"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Users.Create"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Users.Get"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Users.List"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Users.PasswordReset.Create"),
    trellis_rs::generated::OptionalAction::rpc("trellis.auth@v1", "Users.Update"),
    trellis_rs::generated::OptionalAction::subscribe_event(
        "trellis.auth@v1",
        "AuthorizationSessions.Retired",
    ),
    trellis_rs::generated::OptionalAction::subscribe_event("trellis.auth@v1", "Connections.Closed"),
    trellis_rs::generated::OptionalAction::subscribe_event("trellis.auth@v1", "Connections.Kicked"),
    trellis_rs::generated::OptionalAction::subscribe_event("trellis.auth@v1", "Connections.Opened"),
    trellis_rs::generated::OptionalAction::subscribe_event("trellis.auth@v1", "Issuers.Revoked"),
    trellis_rs::generated::OptionalAction::subscribe_event("trellis.auth@v1", "Issuers.Rotated"),
];
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self {
            inner: inner.with_optional_actions(OPTIONAL_ACTIONS),
        }
    }
    pub fn trellis_auth_v1(&self) -> trellis_auth_v1::Client {
        trellis_auth_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_core_v1(&self) -> trellis_core_v1::Client {
        trellis_core_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_events_v1(&self) -> trellis_events_v1::Client {
        trellis_events_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_health_v1(&self) -> trellis_health_v1::Client {
        trellis_health_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_jobs_v1(&self) -> trellis_jobs_v1::Client {
        trellis_jobs_v1::Client::from_generated(self.inner.clone())
    }
    pub fn trellis_state_v1(&self) -> trellis_state_v1::Client {
        trellis_state_v1::Client::from_generated(self.inner.clone())
    }
}
pub mod trellis_auth_v1 {
    use crate::apis::trellis_auth_v1::events;
    use crate::apis::trellis_auth_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn apis_accept(
            &self,
            input: &rpc::ApisAcceptInput,
        ) -> Result<rpc::ApisAcceptOutput, trellis_rs::client::CallError<rpc::ApisAcceptError>>
        {
            self.inner.call::<rpc::ApisAccept>(input).await
        }
        pub async fn apis_force_replace(
            &self,
            input: &rpc::ApisForceReplaceInput,
        ) -> Result<
            rpc::ApisForceReplaceOutput,
            trellis_rs::client::CallError<rpc::ApisForceReplaceError>,
        > {
            self.inner.call::<rpc::ApisForceReplace>(input).await
        }
        pub async fn apis_get(
            &self,
            input: &rpc::ApisGetInput,
        ) -> Result<rpc::ApisGetOutput, trellis_rs::client::CallError<rpc::ApisGetError>> {
            self.inner.call::<rpc::ApisGet>(input).await
        }
        pub async fn apis_list(
            &self,
            input: &rpc::ApisListInput,
        ) -> Result<rpc::ApisListOutput, trellis_rs::client::CallError<rpc::ApisListError>>
        {
            self.inner.call::<rpc::ApisList>(input).await
        }
        pub fn apis_list_pages(
            &self,
            input: rpc::ApisListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ApisListOutput, crate::PaginationError<rpc::ApisListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .apis_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn apis_list_items(
            &self,
            input: rpc::ApisListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthAcceptedApi,
                crate::PaginationError<rpc::ApisListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthAcceptedApi>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .apis_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn apis_review(
            &self,
            input: &rpc::ApisReviewInput,
        ) -> Result<rpc::ApisReviewOutput, trellis_rs::client::CallError<rpc::ApisReviewError>>
        {
            self.inner.call::<rpc::ApisReview>(input).await
        }
        pub async fn authorization_sessions_list(
            &self,
            input: &rpc::AuthorizationSessionsListInput,
        ) -> Result<
            rpc::AuthorizationSessionsListOutput,
            trellis_rs::client::CallError<rpc::AuthorizationSessionsListError>,
        > {
            self.inner
                .call::<rpc::AuthorizationSessionsList>(input)
                .await
        }
        pub fn authorization_sessions_list_pages(
            &self,
            input: rpc::AuthorizationSessionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::AuthorizationSessionsListOutput,
                crate::PaginationError<rpc::AuthorizationSessionsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .authorization_sessions_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn authorization_sessions_list_items(
            &self,
            input: rpc::AuthorizationSessionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthAuthorizationSession,
                crate::PaginationError<rpc::AuthorizationSessionsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthAuthorizationSession>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .authorization_sessions_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn authorization_sessions_revoke(
            &self,
            input: &rpc::AuthorizationSessionsRevokeInput,
        ) -> Result<
            rpc::AuthorizationSessionsRevokeOutput,
            trellis_rs::client::CallError<rpc::AuthorizationSessionsRevokeError>,
        > {
            self.inner
                .call::<rpc::AuthorizationSessionsRevoke>(input)
                .await
        }
        pub async fn capabilities_list(
            &self,
            input: &rpc::CapabilitiesListInput,
        ) -> Result<
            rpc::CapabilitiesListOutput,
            trellis_rs::client::CallError<rpc::CapabilitiesListError>,
        > {
            self.inner.call::<rpc::CapabilitiesList>(input).await
        }
        pub fn capabilities_list_pages(
            &self,
            input: rpc::CapabilitiesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::CapabilitiesListOutput, crate::PaginationError<rpc::CapabilitiesListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .capabilities_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn capabilities_list_items(
            &self,
            input: rpc::CapabilitiesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthCapabilityDefinition,
                crate::PaginationError<rpc::CapabilitiesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthCapabilityDefinition>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .capabilities_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn capability_grants_grant(
            &self,
            input: &rpc::CapabilityGrantsGrantInput,
        ) -> Result<
            rpc::CapabilityGrantsGrantOutput,
            trellis_rs::client::CallError<rpc::CapabilityGrantsGrantError>,
        > {
            self.inner.call::<rpc::CapabilityGrantsGrant>(input).await
        }
        pub async fn capability_grants_list(
            &self,
            input: &rpc::CapabilityGrantsListInput,
        ) -> Result<
            rpc::CapabilityGrantsListOutput,
            trellis_rs::client::CallError<rpc::CapabilityGrantsListError>,
        > {
            self.inner.call::<rpc::CapabilityGrantsList>(input).await
        }
        pub fn capability_grants_list_pages(
            &self,
            input: rpc::CapabilityGrantsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::CapabilityGrantsListOutput,
                crate::PaginationError<rpc::CapabilityGrantsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .capability_grants_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn capability_grants_list_items(
            &self,
            input: rpc::CapabilityGrantsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthCapabilityGrant,
                crate::PaginationError<rpc::CapabilityGrantsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthCapabilityGrant>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .capability_grants_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn capability_grants_revoke(
            &self,
            input: &rpc::CapabilityGrantsRevokeInput,
        ) -> Result<
            rpc::CapabilityGrantsRevokeOutput,
            trellis_rs::client::CallError<rpc::CapabilityGrantsRevokeError>,
        > {
            self.inner.call::<rpc::CapabilityGrantsRevoke>(input).await
        }
        pub async fn connections_kick(
            &self,
            input: &rpc::ConnectionsKickInput,
        ) -> Result<
            rpc::ConnectionsKickOutput,
            trellis_rs::client::CallError<rpc::ConnectionsKickError>,
        > {
            self.inner.call::<rpc::ConnectionsKick>(input).await
        }
        pub async fn connections_list(
            &self,
            input: &rpc::ConnectionsListInput,
        ) -> Result<
            rpc::ConnectionsListOutput,
            trellis_rs::client::CallError<rpc::ConnectionsListError>,
        > {
            self.inner.call::<rpc::ConnectionsList>(input).await
        }
        pub fn connections_list_pages(
            &self,
            input: rpc::ConnectionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ConnectionsListOutput, crate::PaginationError<rpc::ConnectionsListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .connections_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn connections_list_items(
            &self,
            input: rpc::ConnectionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthAttachment,
                crate::PaginationError<rpc::ConnectionsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthAttachment>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .connections_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn delegations_list(
            &self,
            input: &rpc::DelegationsListInput,
        ) -> Result<
            rpc::DelegationsListOutput,
            trellis_rs::client::CallError<rpc::DelegationsListError>,
        > {
            self.inner.call::<rpc::DelegationsList>(input).await
        }
        pub fn delegations_list_pages(
            &self,
            input: rpc::DelegationsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::DelegationsListOutput, crate::PaginationError<rpc::DelegationsListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .delegations_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn delegations_list_items(
            &self,
            input: rpc::DelegationsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthDelegation,
                crate::PaginationError<rpc::DelegationsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthDelegation>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .delegations_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn delegations_review(
            &self,
            input: &rpc::DelegationsReviewInput,
        ) -> Result<
            rpc::DelegationsReviewOutput,
            trellis_rs::client::CallError<rpc::DelegationsReviewError>,
        > {
            self.inner.call::<rpc::DelegationsReview>(input).await
        }
        pub async fn delegations_revoke(
            &self,
            input: &rpc::DelegationsRevokeInput,
        ) -> Result<
            rpc::DelegationsRevokeOutput,
            trellis_rs::client::CallError<rpc::DelegationsRevokeError>,
        > {
            self.inner.call::<rpc::DelegationsRevoke>(input).await
        }
        pub async fn deployments_apply(
            &self,
            input: &rpc::DeploymentsApplyInput,
        ) -> Result<
            rpc::DeploymentsApplyOutput,
            trellis_rs::client::CallError<rpc::DeploymentsApplyError>,
        > {
            self.inner.call::<rpc::DeploymentsApply>(input).await
        }
        pub async fn deployments_create(
            &self,
            input: &rpc::DeploymentsCreateInput,
        ) -> Result<
            rpc::DeploymentsCreateOutput,
            trellis_rs::client::CallError<rpc::DeploymentsCreateError>,
        > {
            self.inner.call::<rpc::DeploymentsCreate>(input).await
        }
        pub async fn deployments_disable(
            &self,
            input: &rpc::DeploymentsDisableInput,
        ) -> Result<
            rpc::DeploymentsDisableOutput,
            trellis_rs::client::CallError<rpc::DeploymentsDisableError>,
        > {
            self.inner.call::<rpc::DeploymentsDisable>(input).await
        }
        pub async fn deployments_enable(
            &self,
            input: &rpc::DeploymentsEnableInput,
        ) -> Result<
            rpc::DeploymentsEnableOutput,
            trellis_rs::client::CallError<rpc::DeploymentsEnableError>,
        > {
            self.inner.call::<rpc::DeploymentsEnable>(input).await
        }
        pub async fn deployments_get(
            &self,
            input: &rpc::DeploymentsGetInput,
        ) -> Result<
            rpc::DeploymentsGetOutput,
            trellis_rs::client::CallError<rpc::DeploymentsGetError>,
        > {
            self.inner.call::<rpc::DeploymentsGet>(input).await
        }
        pub async fn deployments_list(
            &self,
            input: &rpc::DeploymentsListInput,
        ) -> Result<
            rpc::DeploymentsListOutput,
            trellis_rs::client::CallError<rpc::DeploymentsListError>,
        > {
            self.inner.call::<rpc::DeploymentsList>(input).await
        }
        pub fn deployments_list_pages(
            &self,
            input: rpc::DeploymentsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::DeploymentsListOutput, crate::PaginationError<rpc::DeploymentsListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .deployments_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn deployments_list_items(
            &self,
            input: rpc::DeploymentsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthDeployment,
                crate::PaginationError<rpc::DeploymentsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthDeployment>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .deployments_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn deployments_remove(
            &self,
            input: &rpc::DeploymentsRemoveInput,
        ) -> Result<
            rpc::DeploymentsRemoveOutput,
            trellis_rs::client::CallError<rpc::DeploymentsRemoveError>,
        > {
            self.inner.call::<rpc::DeploymentsRemove>(input).await
        }
        pub async fn devices_disable(
            &self,
            input: &rpc::DevicesDisableInput,
        ) -> Result<
            rpc::DevicesDisableOutput,
            trellis_rs::client::CallError<rpc::DevicesDisableError>,
        > {
            self.inner.call::<rpc::DevicesDisable>(input).await
        }
        pub async fn devices_enable(
            &self,
            input: &rpc::DevicesEnableInput,
        ) -> Result<rpc::DevicesEnableOutput, trellis_rs::client::CallError<rpc::DevicesEnableError>>
        {
            self.inner.call::<rpc::DevicesEnable>(input).await
        }
        pub async fn devices_list(
            &self,
            input: &rpc::DevicesListInput,
        ) -> Result<rpc::DevicesListOutput, trellis_rs::client::CallError<rpc::DevicesListError>>
        {
            self.inner.call::<rpc::DevicesList>(input).await
        }
        pub fn devices_list_pages(
            &self,
            input: rpc::DevicesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::DevicesListOutput, crate::PaginationError<rpc::DevicesListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .devices_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn devices_list_items(
            &self,
            input: rpc::DevicesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthProvisionedInstance,
                crate::PaginationError<rpc::DevicesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthProvisionedInstance>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .devices_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn devices_provision(
            &self,
            input: &rpc::DevicesProvisionInput,
        ) -> Result<
            rpc::DevicesProvisionOutput,
            trellis_rs::client::CallError<rpc::DevicesProvisionError>,
        > {
            self.inner.call::<rpc::DevicesProvision>(input).await
        }
        pub async fn devices_remove(
            &self,
            input: &rpc::DevicesRemoveInput,
        ) -> Result<rpc::DevicesRemoveOutput, trellis_rs::client::CallError<rpc::DevicesRemoveError>>
        {
            self.inner.call::<rpc::DevicesRemove>(input).await
        }
        pub async fn issuers_revoke(
            &self,
            input: &rpc::IssuersRevokeInput,
        ) -> Result<rpc::IssuersRevokeOutput, trellis_rs::client::CallError<rpc::IssuersRevokeError>>
        {
            self.inner.call::<rpc::IssuersRevoke>(input).await
        }
        pub async fn issuers_rotate(
            &self,
            input: &rpc::IssuersRotateInput,
        ) -> Result<rpc::IssuersRotateOutput, trellis_rs::client::CallError<rpc::IssuersRotateError>>
        {
            self.inner.call::<rpc::IssuersRotate>(input).await
        }
        pub async fn o_auth_clients_delete(
            &self,
            input: &rpc::OAuthClientsDeleteInput,
        ) -> Result<
            rpc::OAuthClientsDeleteOutput,
            trellis_rs::client::CallError<rpc::OAuthClientsDeleteError>,
        > {
            self.inner.call::<rpc::OAuthClientsDelete>(input).await
        }
        pub async fn o_auth_clients_disable(
            &self,
            input: &rpc::OAuthClientsDisableInput,
        ) -> Result<
            rpc::OAuthClientsDisableOutput,
            trellis_rs::client::CallError<rpc::OAuthClientsDisableError>,
        > {
            self.inner.call::<rpc::OAuthClientsDisable>(input).await
        }
        pub async fn o_auth_clients_get(
            &self,
            input: &rpc::OAuthClientsGetInput,
        ) -> Result<
            rpc::OAuthClientsGetOutput,
            trellis_rs::client::CallError<rpc::OAuthClientsGetError>,
        > {
            self.inner.call::<rpc::OAuthClientsGet>(input).await
        }
        pub async fn o_auth_clients_list(
            &self,
            input: &rpc::OAuthClientsListInput,
        ) -> Result<
            rpc::OAuthClientsListOutput,
            trellis_rs::client::CallError<rpc::OAuthClientsListError>,
        > {
            self.inner.call::<rpc::OAuthClientsList>(input).await
        }
        pub fn o_auth_clients_list_pages(
            &self,
            input: rpc::OAuthClientsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::OAuthClientsListOutput, crate::PaginationError<rpc::OAuthClientsListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .o_auth_clients_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn o_auth_clients_list_items(
            &self,
            input: rpc::OAuthClientsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthOAuthClient,
                crate::PaginationError<rpc::OAuthClientsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthOAuthClient>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .o_auth_clients_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn o_auth_clients_put(
            &self,
            input: &rpc::OAuthClientsPutInput,
        ) -> Result<
            rpc::OAuthClientsPutOutput,
            trellis_rs::client::CallError<rpc::OAuthClientsPutError>,
        > {
            self.inner.call::<rpc::OAuthClientsPut>(input).await
        }
        pub async fn oidc_providers_delete(
            &self,
            input: &rpc::OIDCProvidersDeleteInput,
        ) -> Result<
            rpc::OIDCProvidersDeleteOutput,
            trellis_rs::client::CallError<rpc::OIDCProvidersDeleteError>,
        > {
            self.inner.call::<rpc::OIDCProvidersDelete>(input).await
        }
        pub async fn oidc_providers_disable(
            &self,
            input: &rpc::OIDCProvidersDisableInput,
        ) -> Result<
            rpc::OIDCProvidersDisableOutput,
            trellis_rs::client::CallError<rpc::OIDCProvidersDisableError>,
        > {
            self.inner.call::<rpc::OIDCProvidersDisable>(input).await
        }
        pub async fn oidc_providers_get(
            &self,
            input: &rpc::OIDCProvidersGetInput,
        ) -> Result<
            rpc::OIDCProvidersGetOutput,
            trellis_rs::client::CallError<rpc::OIDCProvidersGetError>,
        > {
            self.inner.call::<rpc::OIDCProvidersGet>(input).await
        }
        pub async fn oidc_providers_list(
            &self,
            input: &rpc::OIDCProvidersListInput,
        ) -> Result<
            rpc::OIDCProvidersListOutput,
            trellis_rs::client::CallError<rpc::OIDCProvidersListError>,
        > {
            self.inner.call::<rpc::OIDCProvidersList>(input).await
        }
        pub fn oidc_providers_list_pages(
            &self,
            input: rpc::OIDCProvidersListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::OIDCProvidersListOutput,
                crate::PaginationError<rpc::OIDCProvidersListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .oidc_providers_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn oidc_providers_list_items(
            &self,
            input: rpc::OIDCProvidersListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthOIDCProvider,
                crate::PaginationError<rpc::OIDCProvidersListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthOIDCProvider>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .oidc_providers_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn oidc_providers_put(
            &self,
            input: &rpc::OIDCProvidersPutInput,
        ) -> Result<
            rpc::OIDCProvidersPutOutput,
            trellis_rs::client::CallError<rpc::OIDCProvidersPutError>,
        > {
            self.inner.call::<rpc::OIDCProvidersPut>(input).await
        }
        pub async fn oidc_role_mappings_delete(
            &self,
            input: &rpc::OIDCRoleMappingsDeleteInput,
        ) -> Result<
            rpc::OIDCRoleMappingsDeleteOutput,
            trellis_rs::client::CallError<rpc::OIDCRoleMappingsDeleteError>,
        > {
            self.inner.call::<rpc::OIDCRoleMappingsDelete>(input).await
        }
        pub async fn oidc_role_mappings_list(
            &self,
            input: &rpc::OIDCRoleMappingsListInput,
        ) -> Result<
            rpc::OIDCRoleMappingsListOutput,
            trellis_rs::client::CallError<rpc::OIDCRoleMappingsListError>,
        > {
            self.inner.call::<rpc::OIDCRoleMappingsList>(input).await
        }
        pub fn oidc_role_mappings_list_pages(
            &self,
            input: rpc::OIDCRoleMappingsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::OIDCRoleMappingsListOutput,
                crate::PaginationError<rpc::OIDCRoleMappingsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .oidc_role_mappings_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn oidc_role_mappings_list_items(
            &self,
            input: rpc::OIDCRoleMappingsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthOIDCRoleMapping,
                crate::PaginationError<rpc::OIDCRoleMappingsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthOIDCRoleMapping>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .oidc_role_mappings_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn oidc_role_mappings_put(
            &self,
            input: &rpc::OIDCRoleMappingsPutInput,
        ) -> Result<
            rpc::OIDCRoleMappingsPutOutput,
            trellis_rs::client::CallError<rpc::OIDCRoleMappingsPutError>,
        > {
            self.inner.call::<rpc::OIDCRoleMappingsPut>(input).await
        }
        pub async fn platform_delegations_list(
            &self,
            input: &rpc::PlatformDelegationsListInput,
        ) -> Result<
            rpc::PlatformDelegationsListOutput,
            trellis_rs::client::CallError<rpc::PlatformDelegationsListError>,
        > {
            self.inner.call::<rpc::PlatformDelegationsList>(input).await
        }
        pub fn platform_delegations_list_pages(
            &self,
            input: rpc::PlatformDelegationsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::PlatformDelegationsListOutput,
                crate::PaginationError<rpc::PlatformDelegationsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .platform_delegations_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn platform_delegations_list_items(
            &self,
            input: rpc::PlatformDelegationsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthPlatformDelegation,
                crate::PaginationError<rpc::PlatformDelegationsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthPlatformDelegation>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .platform_delegations_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn platform_delegations_put(
            &self,
            input: &rpc::PlatformDelegationsPutInput,
        ) -> Result<
            rpc::PlatformDelegationsPutOutput,
            trellis_rs::client::CallError<rpc::PlatformDelegationsPutError>,
        > {
            self.inner.call::<rpc::PlatformDelegationsPut>(input).await
        }
        pub async fn platform_delegations_revoke(
            &self,
            input: &rpc::PlatformDelegationsRevokeInput,
        ) -> Result<
            rpc::PlatformDelegationsRevokeOutput,
            trellis_rs::client::CallError<rpc::PlatformDelegationsRevokeError>,
        > {
            self.inner
                .call::<rpc::PlatformDelegationsRevoke>(input)
                .await
        }
        pub async fn platform_privileges_assign(
            &self,
            input: &rpc::PlatformPrivilegesAssignInput,
        ) -> Result<
            rpc::PlatformPrivilegesAssignOutput,
            trellis_rs::client::CallError<rpc::PlatformPrivilegesAssignError>,
        > {
            self.inner
                .call::<rpc::PlatformPrivilegesAssign>(input)
                .await
        }
        pub async fn platform_privileges_list(
            &self,
            input: &rpc::PlatformPrivilegesListInput,
        ) -> Result<
            rpc::PlatformPrivilegesListOutput,
            trellis_rs::client::CallError<rpc::PlatformPrivilegesListError>,
        > {
            self.inner.call::<rpc::PlatformPrivilegesList>(input).await
        }
        pub fn platform_privileges_list_pages(
            &self,
            input: rpc::PlatformPrivilegesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::PlatformPrivilegesListOutput,
                crate::PaginationError<rpc::PlatformPrivilegesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .platform_privileges_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn platform_privileges_list_items(
            &self,
            input: rpc::PlatformPrivilegesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthPlatformAssignment,
                crate::PaginationError<rpc::PlatformPrivilegesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthPlatformAssignment>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .platform_privileges_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn platform_privileges_revoke(
            &self,
            input: &rpc::PlatformPrivilegesRevokeInput,
        ) -> Result<
            rpc::PlatformPrivilegesRevokeOutput,
            trellis_rs::client::CallError<rpc::PlatformPrivilegesRevokeError>,
        > {
            self.inner
                .call::<rpc::PlatformPrivilegesRevoke>(input)
                .await
        }
        pub async fn principals_effective_access(
            &self,
            input: &rpc::PrincipalsEffectiveAccessInput,
        ) -> Result<
            rpc::PrincipalsEffectiveAccessOutput,
            trellis_rs::client::CallError<rpc::PrincipalsEffectiveAccessError>,
        > {
            self.inner
                .call::<rpc::PrincipalsEffectiveAccess>(input)
                .await
        }
        pub async fn resources_get(
            &self,
            input: &rpc::ResourcesGetInput,
        ) -> Result<rpc::ResourcesGetOutput, trellis_rs::client::CallError<rpc::ResourcesGetError>>
        {
            self.inner.call::<rpc::ResourcesGet>(input).await
        }
        pub async fn resources_list(
            &self,
            input: &rpc::ResourcesListInput,
        ) -> Result<rpc::ResourcesListOutput, trellis_rs::client::CallError<rpc::ResourcesListError>>
        {
            self.inner.call::<rpc::ResourcesList>(input).await
        }
        pub fn resources_list_pages(
            &self,
            input: rpc::ResourcesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ResourcesListOutput, crate::PaginationError<rpc::ResourcesListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .resources_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn resources_list_items(
            &self,
            input: rpc::ResourcesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthResourceBinding,
                crate::PaginationError<rpc::ResourcesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthResourceBinding>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .resources_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn resources_remove(
            &self,
            input: &rpc::ResourcesRemoveInput,
        ) -> Result<
            rpc::ResourcesRemoveOutput,
            trellis_rs::client::CallError<rpc::ResourcesRemoveError>,
        > {
            self.inner.call::<rpc::ResourcesRemove>(input).await
        }
        pub async fn role_assignments_assign(
            &self,
            input: &rpc::RoleAssignmentsAssignInput,
        ) -> Result<
            rpc::RoleAssignmentsAssignOutput,
            trellis_rs::client::CallError<rpc::RoleAssignmentsAssignError>,
        > {
            self.inner.call::<rpc::RoleAssignmentsAssign>(input).await
        }
        pub async fn role_assignments_list(
            &self,
            input: &rpc::RoleAssignmentsListInput,
        ) -> Result<
            rpc::RoleAssignmentsListOutput,
            trellis_rs::client::CallError<rpc::RoleAssignmentsListError>,
        > {
            self.inner.call::<rpc::RoleAssignmentsList>(input).await
        }
        pub fn role_assignments_list_pages(
            &self,
            input: rpc::RoleAssignmentsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::RoleAssignmentsListOutput,
                crate::PaginationError<rpc::RoleAssignmentsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .role_assignments_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn role_assignments_list_items(
            &self,
            input: rpc::RoleAssignmentsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthRoleAssignment,
                crate::PaginationError<rpc::RoleAssignmentsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthRoleAssignment>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .role_assignments_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn role_assignments_revoke(
            &self,
            input: &rpc::RoleAssignmentsRevokeInput,
        ) -> Result<
            rpc::RoleAssignmentsRevokeOutput,
            trellis_rs::client::CallError<rpc::RoleAssignmentsRevokeError>,
        > {
            self.inner.call::<rpc::RoleAssignmentsRevoke>(input).await
        }
        pub async fn roles_delete(
            &self,
            input: &rpc::RolesDeleteInput,
        ) -> Result<rpc::RolesDeleteOutput, trellis_rs::client::CallError<rpc::RolesDeleteError>>
        {
            self.inner.call::<rpc::RolesDelete>(input).await
        }
        pub async fn roles_get(
            &self,
            input: &rpc::RolesGetInput,
        ) -> Result<rpc::RolesGetOutput, trellis_rs::client::CallError<rpc::RolesGetError>>
        {
            self.inner.call::<rpc::RolesGet>(input).await
        }
        pub async fn roles_list(
            &self,
            input: &rpc::RolesListInput,
        ) -> Result<rpc::RolesListOutput, trellis_rs::client::CallError<rpc::RolesListError>>
        {
            self.inner.call::<rpc::RolesList>(input).await
        }
        pub fn roles_list_pages(
            &self,
            input: rpc::RolesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::RolesListOutput, crate::PaginationError<rpc::RolesListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .roles_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn roles_list_items(
            &self,
            input: rpc::RolesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<crate::__types::trellis::AuthRole, crate::PaginationError<rpc::RolesListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthRole>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .roles_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn roles_put(
            &self,
            input: &rpc::RolesPutInput,
        ) -> Result<rpc::RolesPutOutput, trellis_rs::client::CallError<rpc::RolesPutError>>
        {
            self.inner.call::<rpc::RolesPut>(input).await
        }
        pub async fn service_instances_disable(
            &self,
            input: &rpc::ServiceInstancesDisableInput,
        ) -> Result<
            rpc::ServiceInstancesDisableOutput,
            trellis_rs::client::CallError<rpc::ServiceInstancesDisableError>,
        > {
            self.inner.call::<rpc::ServiceInstancesDisable>(input).await
        }
        pub async fn service_instances_enable(
            &self,
            input: &rpc::ServiceInstancesEnableInput,
        ) -> Result<
            rpc::ServiceInstancesEnableOutput,
            trellis_rs::client::CallError<rpc::ServiceInstancesEnableError>,
        > {
            self.inner.call::<rpc::ServiceInstancesEnable>(input).await
        }
        pub async fn service_instances_list(
            &self,
            input: &rpc::ServiceInstancesListInput,
        ) -> Result<
            rpc::ServiceInstancesListOutput,
            trellis_rs::client::CallError<rpc::ServiceInstancesListError>,
        > {
            self.inner.call::<rpc::ServiceInstancesList>(input).await
        }
        pub fn service_instances_list_pages(
            &self,
            input: rpc::ServiceInstancesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::ServiceInstancesListOutput,
                crate::PaginationError<rpc::ServiceInstancesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .service_instances_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn service_instances_list_items(
            &self,
            input: rpc::ServiceInstancesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthProvisionedInstance,
                crate::PaginationError<rpc::ServiceInstancesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthProvisionedInstance>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .service_instances_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn service_instances_provision(
            &self,
            input: &rpc::ServiceInstancesProvisionInput,
        ) -> Result<
            rpc::ServiceInstancesProvisionOutput,
            trellis_rs::client::CallError<rpc::ServiceInstancesProvisionError>,
        > {
            self.inner
                .call::<rpc::ServiceInstancesProvision>(input)
                .await
        }
        pub async fn service_instances_remove(
            &self,
            input: &rpc::ServiceInstancesRemoveInput,
        ) -> Result<
            rpc::ServiceInstancesRemoveOutput,
            trellis_rs::client::CallError<rpc::ServiceInstancesRemoveError>,
        > {
            self.inner.call::<rpc::ServiceInstancesRemove>(input).await
        }
        pub async fn sessions_list(
            &self,
            input: &rpc::SessionsListInput,
        ) -> Result<rpc::SessionsListOutput, trellis_rs::client::CallError<rpc::SessionsListError>>
        {
            self.inner.call::<rpc::SessionsList>(input).await
        }
        pub fn sessions_list_pages(
            &self,
            input: rpc::SessionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::SessionsListOutput, crate::PaginationError<rpc::SessionsListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .sessions_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn sessions_list_items(
            &self,
            input: rpc::SessionsListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthLoginSession,
                crate::PaginationError<rpc::SessionsListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthLoginSession>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .sessions_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn sessions_revoke(
            &self,
            input: &rpc::SessionsRevokeInput,
        ) -> Result<
            rpc::SessionsRevokeOutput,
            trellis_rs::client::CallError<rpc::SessionsRevokeError>,
        > {
            self.inner.call::<rpc::SessionsRevoke>(input).await
        }
        pub async fn user_identities_list(
            &self,
            input: &rpc::UserIdentitiesListInput,
        ) -> Result<
            rpc::UserIdentitiesListOutput,
            trellis_rs::client::CallError<rpc::UserIdentitiesListError>,
        > {
            self.inner.call::<rpc::UserIdentitiesList>(input).await
        }
        pub fn user_identities_list_pages(
            &self,
            input: rpc::UserIdentitiesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                rpc::UserIdentitiesListOutput,
                crate::PaginationError<rpc::UserIdentitiesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .user_identities_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn user_identities_list_items(
            &self,
            input: rpc::UserIdentitiesListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::AuthUserIdentity,
                crate::PaginationError<rpc::UserIdentitiesListError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthUserIdentity>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .user_identities_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn user_identities_unlink(
            &self,
            input: &rpc::UserIdentitiesUnlinkInput,
        ) -> Result<
            rpc::UserIdentitiesUnlinkOutput,
            trellis_rs::client::CallError<rpc::UserIdentitiesUnlinkError>,
        > {
            self.inner.call::<rpc::UserIdentitiesUnlink>(input).await
        }
        pub async fn users_create(
            &self,
            input: &rpc::UsersCreateInput,
        ) -> Result<rpc::UsersCreateOutput, trellis_rs::client::CallError<rpc::UsersCreateError>>
        {
            self.inner.call::<rpc::UsersCreate>(input).await
        }
        pub async fn users_get(
            &self,
            input: &rpc::UsersGetInput,
        ) -> Result<rpc::UsersGetOutput, trellis_rs::client::CallError<rpc::UsersGetError>>
        {
            self.inner.call::<rpc::UsersGet>(input).await
        }
        pub async fn users_identity_link_create(
            &self,
            input: &rpc::UsersIdentityLinkCreateInput,
        ) -> Result<
            rpc::UsersIdentityLinkCreateOutput,
            trellis_rs::client::CallError<rpc::UsersIdentityLinkCreateError>,
        > {
            self.inner.call::<rpc::UsersIdentityLinkCreate>(input).await
        }
        pub async fn users_list(
            &self,
            input: &rpc::UsersListInput,
        ) -> Result<rpc::UsersListOutput, trellis_rs::client::CallError<rpc::UsersListError>>
        {
            self.inner.call::<rpc::UsersList>(input).await
        }
        pub fn users_list_pages(
            &self,
            input: rpc::UsersListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::UsersListOutput, crate::PaginationError<rpc::UsersListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .users_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn users_list_items(
            &self,
            input: rpc::UsersListInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<crate::__types::trellis::AuthUser, crate::PaginationError<rpc::UsersListError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::AuthUser>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .users_list(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn users_password_change(
            &self,
            input: &rpc::UsersPasswordChangeInput,
        ) -> Result<
            rpc::UsersPasswordChangeOutput,
            trellis_rs::client::CallError<rpc::UsersPasswordChangeError>,
        > {
            self.inner.call::<rpc::UsersPasswordChange>(input).await
        }
        pub async fn users_password_reset_create(
            &self,
            input: &rpc::UsersPasswordResetCreateInput,
        ) -> Result<
            rpc::UsersPasswordResetCreateOutput,
            trellis_rs::client::CallError<rpc::UsersPasswordResetCreateError>,
        > {
            self.inner
                .call::<rpc::UsersPasswordResetCreate>(input)
                .await
        }
        pub async fn users_resolve(
            &self,
            input: &rpc::UsersResolveInput,
        ) -> Result<rpc::UsersResolveOutput, trellis_rs::client::CallError<rpc::UsersResolveError>>
        {
            self.inner.call::<rpc::UsersResolve>(input).await
        }
        pub async fn users_update(
            &self,
            input: &rpc::UsersUpdateInput,
        ) -> Result<rpc::UsersUpdateOutput, trellis_rs::client::CallError<rpc::UsersUpdateError>>
        {
            self.inner.call::<rpc::UsersUpdate>(input).await
        }
        pub async fn subscribe_authorization_sessions_retired(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<
                    events::AuthorizationSessionsRetiredEvent,
                    trellis_rs::client::TrellisClientError,
                >,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::AuthorizationSessionsRetired>(options)
                .await
        }
        pub async fn subscribe_connections_closed(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::ConnectionsClosedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::ConnectionsClosed>(options)
                .await
        }
        pub async fn subscribe_connections_kicked(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::ConnectionsKickedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::ConnectionsKicked>(options)
                .await
        }
        pub async fn subscribe_connections_opened(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::ConnectionsOpenedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::ConnectionsOpened>(options)
                .await
        }
        pub async fn subscribe_issuers_revoked(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::IssuersRevokedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::IssuersRevoked>(options)
                .await
        }
        pub async fn subscribe_issuers_rotated(
            &self,
            options: trellis_rs::client::EventSubscribeOptions,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<events::IssuersRotatedEvent, trellis_rs::client::TrellisClientError>,
            >,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner
                .subscribe::<events::IssuersRotated>(options)
                .await
        }
    }
}
pub mod trellis_core_v1 {
    use crate::apis::trellis_core_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn resources_destroy(
            &self,
            input: &rpc::ResourcesDestroyInput,
        ) -> Result<
            rpc::ResourcesDestroyOutput,
            trellis_rs::client::CallError<rpc::ResourcesDestroyError>,
        > {
            self.inner.call::<rpc::ResourcesDestroy>(input).await
        }
        pub async fn resources_inspect(
            &self,
            input: &rpc::ResourcesInspectInput,
        ) -> Result<
            rpc::ResourcesInspectOutput,
            trellis_rs::client::CallError<rpc::ResourcesInspectError>,
        > {
            self.inner.call::<rpc::ResourcesInspect>(input).await
        }
        pub async fn resources_query(
            &self,
            input: &rpc::ResourcesQueryInput,
        ) -> Result<
            rpc::ResourcesQueryOutput,
            trellis_rs::client::CallError<rpc::ResourcesQueryError>,
        > {
            self.inner.call::<rpc::ResourcesQuery>(input).await
        }
        pub fn resources_query_pages(
            &self,
            input: rpc::ResourcesQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ResourcesQueryOutput, crate::PaginationError<rpc::ResourcesQueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .resources_query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn resources_query_items(
            &self,
            input: rpc::ResourcesQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::ResourceInspection,
                crate::PaginationError<rpc::ResourcesQueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::ResourceInspection>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .resources_query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
    }
}
pub mod trellis_events_v1 {
    use crate::apis::trellis_events_v1::lives;
    use crate::apis::trellis_events_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn consumers_inspect(
            &self,
            input: &rpc::ConsumersInspectInput,
        ) -> Result<
            rpc::ConsumersInspectOutput,
            trellis_rs::client::CallError<rpc::ConsumersInspectError>,
        > {
            self.inner.call::<rpc::ConsumersInspect>(input).await
        }
        pub async fn consumers_query(
            &self,
            input: &rpc::ConsumersQueryInput,
        ) -> Result<
            rpc::ConsumersQueryOutput,
            trellis_rs::client::CallError<rpc::ConsumersQueryError>,
        > {
            self.inner.call::<rpc::ConsumersQuery>(input).await
        }
        pub fn consumers_query_pages(
            &self,
            input: rpc::ConsumersQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ConsumersQueryOutput, crate::PaginationError<rpc::ConsumersQueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .consumers_query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn consumers_query_items(
            &self,
            input: rpc::ConsumersQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::EventConsumerStatusRow,
                crate::PaginationError<rpc::ConsumersQueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::EventConsumerStatusRow>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .consumers_query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn dead_letters_dismiss(
            &self,
            input: &rpc::DeadLettersDismissInput,
        ) -> Result<
            rpc::DeadLettersDismissOutput,
            trellis_rs::client::CallError<rpc::DeadLettersDismissError>,
        > {
            self.inner.call::<rpc::DeadLettersDismiss>(input).await
        }
        pub async fn dead_letters_inspect(
            &self,
            input: &rpc::DeadLettersInspectInput,
        ) -> Result<
            rpc::DeadLettersInspectOutput,
            trellis_rs::client::CallError<rpc::DeadLettersInspectError>,
        > {
            self.inner.call::<rpc::DeadLettersInspect>(input).await
        }
        pub async fn dead_letters_query(
            &self,
            input: &rpc::DeadLettersQueryInput,
        ) -> Result<
            rpc::DeadLettersQueryOutput,
            trellis_rs::client::CallError<rpc::DeadLettersQueryError>,
        > {
            self.inner.call::<rpc::DeadLettersQuery>(input).await
        }
        pub fn dead_letters_query_pages(
            &self,
            input: rpc::DeadLettersQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::DeadLettersQueryOutput, crate::PaginationError<rpc::DeadLettersQueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .dead_letters_query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn dead_letters_query_items(
            &self,
            input: rpc::DeadLettersQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::EventsDeadLetter,
                crate::PaginationError<rpc::DeadLettersQueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::EventsDeadLetter>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .dead_letters_query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn dead_letters_replay(
            &self,
            input: &rpc::DeadLettersReplayInput,
        ) -> Result<
            rpc::DeadLettersReplayOutput,
            trellis_rs::client::CallError<rpc::DeadLettersReplayError>,
        > {
            self.inner.call::<rpc::DeadLettersReplay>(input).await
        }
        pub async fn diagnostics(
            &self,
            input: &rpc::DiagnosticsInput,
        ) -> Result<rpc::DiagnosticsOutput, trellis_rs::client::CallError<rpc::DiagnosticsError>>
        {
            self.inner.call::<rpc::Diagnostics>(input).await
        }
        pub async fn inspect(
            &self,
            input: &rpc::InspectInput,
        ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
            self.inner.call::<rpc::Inspect>(input).await
        }
        pub async fn metrics(
            &self,
            input: &rpc::MetricsInput,
        ) -> Result<rpc::MetricsOutput, trellis_rs::client::CallError<rpc::MetricsError>> {
            self.inner.call::<rpc::Metrics>(input).await
        }
        pub async fn query(
            &self,
            input: &rpc::QueryInput,
        ) -> Result<rpc::QueryOutput, trellis_rs::client::CallError<rpc::QueryError>> {
            self.inner.call::<rpc::Query>(input).await
        }
        pub fn query_pages(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::QueryOutput, crate::PaginationError<rpc::QueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn query_items(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<crate::__types::trellis::EventsRow, crate::PaginationError<rpc::QueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::EventsRow>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn watch(
            &self,
            input: &lives::WatchInput,
        ) -> Result<
            trellis_rs::LiveSubscription<lives::WatchEvent>,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.live::<lives::Watch>(input).await
        }
    }
}
pub mod trellis_health_v1 {
    use crate::apis::trellis_health_v1::lives;
    use crate::apis::trellis_health_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn inspect(
            &self,
            input: &rpc::InspectInput,
        ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
            self.inner.call::<rpc::Inspect>(input).await
        }
        pub async fn metrics(
            &self,
            input: &rpc::MetricsInput,
        ) -> Result<rpc::MetricsOutput, trellis_rs::client::CallError<rpc::MetricsError>> {
            self.inner.call::<rpc::Metrics>(input).await
        }
        pub async fn query(
            &self,
            input: &rpc::QueryInput,
        ) -> Result<rpc::QueryOutput, trellis_rs::client::CallError<rpc::QueryError>> {
            self.inner.call::<rpc::Query>(input).await
        }
        pub fn query_pages(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::QueryOutput, crate::PaginationError<rpc::QueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn query_items(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::HealthQueryResponseentriesItem,
                crate::PaginationError<rpc::QueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::HealthQueryResponseentriesItem>::new()
                        .into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn summary(
            &self,
            input: &rpc::SummaryInput,
        ) -> Result<rpc::SummaryOutput, trellis_rs::client::CallError<rpc::SummaryError>> {
            self.inner.call::<rpc::Summary>(input).await
        }
        pub async fn watch(
            &self,
            input: &lives::WatchInput,
        ) -> Result<
            trellis_rs::LiveSubscription<lives::WatchEvent>,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.live::<lives::Watch>(input).await
        }
    }
}
pub mod trellis_jobs_v1 {
    use crate::apis::trellis_jobs_v1::lives;
    use crate::apis::trellis_jobs_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn cancel(
            &self,
            input: &rpc::CancelInput,
        ) -> Result<rpc::CancelOutput, trellis_rs::client::CallError<rpc::CancelError>> {
            self.inner.call::<rpc::Cancel>(input).await
        }
        pub async fn dismiss_dlq(
            &self,
            input: &rpc::DismissDLQInput,
        ) -> Result<rpc::DismissDLQOutput, trellis_rs::client::CallError<rpc::DismissDLQError>>
        {
            self.inner.call::<rpc::DismissDLQ>(input).await
        }
        pub async fn get_key(
            &self,
            input: &rpc::GetKeyInput,
        ) -> Result<rpc::GetKeyOutput, trellis_rs::client::CallError<rpc::GetKeyError>> {
            self.inner.call::<rpc::GetKey>(input).await
        }
        pub async fn inspect(
            &self,
            input: &rpc::InspectInput,
        ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
            self.inner.call::<rpc::Inspect>(input).await
        }
        pub async fn list_dlq(
            &self,
            input: &rpc::ListDLQInput,
        ) -> Result<rpc::ListDLQOutput, trellis_rs::client::CallError<rpc::ListDLQError>> {
            self.inner.call::<rpc::ListDLQ>(input).await
        }
        pub fn list_dlq_pages(
            &self,
            input: rpc::ListDLQInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ListDLQOutput, crate::PaginationError<rpc::ListDLQError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .list_dlq(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn list_dlq_items(
            &self,
            input: rpc::ListDLQInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::JobsListDLQResponseentriesItem,
                crate::PaginationError<rpc::ListDLQError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::JobsListDLQResponseentriesItem>::new()
                        .into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .list_dlq(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn list_services(
            &self,
            input: &rpc::ListServicesInput,
        ) -> Result<rpc::ListServicesOutput, trellis_rs::client::CallError<rpc::ListServicesError>>
        {
            self.inner.call::<rpc::ListServices>(input).await
        }
        pub fn list_services_pages(
            &self,
            input: rpc::ListServicesInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ListServicesOutput, crate::PaginationError<rpc::ListServicesError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .list_services(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn list_services_items(
            &self,
            input: rpc::ListServicesInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::JobsListServicesResponseentriesItem,
                crate::PaginationError<rpc::ListServicesError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::JobsListServicesResponseentriesItem>::new()
                        .into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .list_services(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn metrics(
            &self,
            input: &rpc::MetricsInput,
        ) -> Result<rpc::MetricsOutput, trellis_rs::client::CallError<rpc::MetricsError>> {
            self.inner.call::<rpc::Metrics>(input).await
        }
        pub async fn query(
            &self,
            input: &rpc::QueryInput,
        ) -> Result<rpc::QueryOutput, trellis_rs::client::CallError<rpc::QueryError>> {
            self.inner.call::<rpc::Query>(input).await
        }
        pub fn query_pages(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::QueryOutput, crate::PaginationError<rpc::QueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn query_items(
            &self,
            input: rpc::QueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::JobsQueryResponseentriesItem,
                crate::PaginationError<rpc::QueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::JobsQueryResponseentriesItem>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
        pub async fn replay_dlq(
            &self,
            input: &rpc::ReplayDLQInput,
        ) -> Result<rpc::ReplayDLQOutput, trellis_rs::client::CallError<rpc::ReplayDLQError>>
        {
            self.inner.call::<rpc::ReplayDLQ>(input).await
        }
        pub async fn retry(
            &self,
            input: &rpc::RetryInput,
        ) -> Result<rpc::RetryOutput, trellis_rs::client::CallError<rpc::RetryError>> {
            self.inner.call::<rpc::Retry>(input).await
        }
        pub async fn summary(
            &self,
            input: &rpc::SummaryInput,
        ) -> Result<rpc::SummaryOutput, trellis_rs::client::CallError<rpc::SummaryError>> {
            self.inner.call::<rpc::Summary>(input).await
        }
        pub async fn watch(
            &self,
            input: &lives::WatchInput,
        ) -> Result<
            trellis_rs::LiveSubscription<lives::WatchEvent>,
            trellis_rs::client::TrellisClientError,
        > {
            self.inner.live::<lives::Watch>(input).await
        }
    }
}
pub mod trellis_state_v1 {
    use crate::apis::trellis_state_v1::rpc;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub async fn delete(
            &self,
            input: &rpc::DeleteInput,
        ) -> Result<rpc::DeleteOutput, trellis_rs::client::CallError<rpc::DeleteError>> {
            self.inner.call::<rpc::Delete>(input).await
        }
        pub async fn get(
            &self,
            input: &rpc::GetInput,
        ) -> Result<rpc::GetOutput, trellis_rs::client::CallError<rpc::GetError>> {
            self.inner.call::<rpc::Get>(input).await
        }
        pub async fn put(
            &self,
            input: &rpc::PutInput,
        ) -> Result<rpc::PutOutput, trellis_rs::client::CallError<rpc::PutError>> {
            self.inner.call::<rpc::Put>(input).await
        }
        pub async fn resources_inspect(
            &self,
            input: &rpc::ResourcesInspectInput,
        ) -> Result<
            rpc::ResourcesInspectOutput,
            trellis_rs::client::CallError<rpc::ResourcesInspectError>,
        > {
            self.inner.call::<rpc::ResourcesInspect>(input).await
        }
        pub async fn resources_query(
            &self,
            input: &rpc::ResourcesQueryInput,
        ) -> Result<
            rpc::ResourcesQueryOutput,
            trellis_rs::client::CallError<rpc::ResourcesQueryError>,
        > {
            self.inner.call::<rpc::ResourcesQuery>(input).await
        }
        pub fn resources_query_pages(
            &self,
            input: rpc::ResourcesQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<rpc::ResourcesQueryOutput, crate::PaginationError<rpc::ResourcesQueryError>>,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (client, input, seen, false),
                |(client, mut input, mut seen, done)| async move {
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .resources_query(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    let done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    Ok(Some((page, (client, input, seen, done))))
                },
            ))
        }
        pub fn resources_query_items(
            &self,
            input: rpc::ResourcesQueryInput,
        ) -> futures_util::stream::BoxStream<
            'static,
            Result<
                crate::__types::trellis::ResourceInspection,
                crate::PaginationError<rpc::ResourcesQueryError>,
            >,
        > {
            let client = self.clone();
            let mut seen = std::collections::BTreeSet::new();
            if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
                seen.insert(cursor);
            }
            Box::pin(futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<crate::__types::trellis::ResourceInspection>::new().into_iter(),
                ),
                |(client, mut input, mut seen, mut done, mut items)| async move {
                    loop {
                        if let Some(item) = items.next() {
                            return Ok(Some((item, (client, input, seen, done, items))));
                        }
                        if done {
                            return Ok(None);
                        }
                        let page = client
                            .resources_query(&input)
                            .await
                            .map_err(crate::PaginationError::Call)?;
                        let next = page.page.next_cursor.clone();
                        done = next.is_none();
                        if let Some(cursor) = next {
                            if !seen.insert(cursor.clone()) {
                                return Err(crate::PaginationError::RepeatedCursor(cursor));
                            }
                            let limit = input.page.as_ref().and_then(|page| page.limit);
                            input.page = Some(crate::__types::CursorQuery {
                                cursor: Some(cursor),
                                limit,
                            });
                        }
                        items = page.items.into_iter();
                    }
                },
            ))
        }
    }
}
pub mod types {
    //! Generated wire type exports.
}
