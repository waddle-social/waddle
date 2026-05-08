use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::OnceCell;
use waddle_extensions::host_tools as ext_host;

#[derive(Default)]
pub(crate) struct DeferredExtensionHostTools {
    inner: OnceCell<Arc<dyn ext_host::ExtensionHostTools>>,
}

impl DeferredExtensionHostTools {
    pub(crate) fn set(&self, tools: Arc<dyn ext_host::ExtensionHostTools>) {
        let _ = self.inner.set(tools);
    }

    fn tools(
        &self,
    ) -> std::result::Result<&Arc<dyn ext_host::ExtensionHostTools>, ext_host::HostToolError> {
        self.inner.get().ok_or_else(|| ext_host::HostToolError {
            code: ext_host::HostToolErrorCode::Unsupported,
            message: waddle_extensions::DisplayText::new(
                "extension host tools are not wired into the server",
            )
            .expect("static host-tool error is non-empty"),
        })
    }
}

#[async_trait]
impl ext_host::ExtensionHostTools for DeferredExtensionHostTools {
    async fn list_channels(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListChannelsRequest,
    ) -> std::result::Result<ext_host::ListChannelsResponse, ext_host::HostToolError> {
        self.tools()?.list_channels(context, request).await
    }

    async fn list_spaces(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListSpacesRequest,
    ) -> std::result::Result<ext_host::ListSpacesResponse, ext_host::HostToolError> {
        self.tools()?.list_spaces(context, request).await
    }

    async fn list_room_members(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListRoomMembersRequest,
    ) -> std::result::Result<ext_host::ListRoomMembersResponse, ext_host::HostToolError> {
        self.tools()?.list_room_members(context, request).await
    }

    async fn get_presence(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetPresenceRequest,
    ) -> std::result::Result<ext_host::GetPresenceResponse, ext_host::HostToolError> {
        self.tools()?.get_presence(context, request).await
    }

    async fn get_roster(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetRosterRequest,
    ) -> std::result::Result<ext_host::GetRosterResponse, ext_host::HostToolError> {
        self.tools()?.get_roster(context, request).await
    }

    async fn query_mam(
        &self,
        context: &ext_host::InvocationContext,
        query: ext_host::MamQuery,
    ) -> std::result::Result<ext_host::MamQueryResponse, ext_host::HostToolError> {
        self.tools()?.query_mam(context, query).await
    }

    async fn send_message(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::SendMessageRequest,
    ) -> std::result::Result<ext_host::SendMessageResponse, ext_host::HostToolError> {
        self.tools()?.send_message(context, request).await
    }
}
