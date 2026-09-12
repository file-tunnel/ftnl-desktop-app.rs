//! Shared file-transfer feature-parity contract with `file-tunnel/ftnl-flutter`.
//! Native transport, filesystem, lifecycle, and notification behavior belongs
//! only in [`AppPlatformAdapter`].
pub const CROSS_PLATFORM_PARITY_CONTRACT_VERSION: u32 = 1;
pub const FLUTTER_COUNTERPART: &str = "file-tunnel/ftnl-flutter";
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AppSurface { Mobile, FlutterDesktop, RustDesktop }
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AppCapability {
    Authentication, PeerDiscovery, EncryptedTransfer, SendFiles, ReceiveFiles,
    TransferResume, CollisionHandling, FilePicker, ShareIntent, DeepLinks,
    BackgroundTransfer, Notifications, OfflineQueue, Telemetry, Accessibility,
    ApplicationUpdates,
}
pub const REQUIRED_PARITY_CAPABILITIES: &[AppCapability] = &[
    AppCapability::Authentication, AppCapability::PeerDiscovery,
    AppCapability::EncryptedTransfer, AppCapability::SendFiles,
    AppCapability::ReceiveFiles, AppCapability::TransferResume,
    AppCapability::CollisionHandling, AppCapability::FilePicker,
    AppCapability::ShareIntent, AppCapability::DeepLinks,
    AppCapability::BackgroundTransfer, AppCapability::Notifications,
    AppCapability::OfflineQueue, AppCapability::Telemetry,
    AppCapability::Accessibility, AppCapability::ApplicationUpdates,
];
pub trait AppPlatformAdapter {
    fn surface(&self) -> AppSurface;
    fn supports(&self, capability: AppCapability) -> bool;
}
pub fn verify_required_parity_capabilities(
    adapter: &impl AppPlatformAdapter,
) -> Result<(), Vec<AppCapability>> {
    let missing = REQUIRED_PARITY_CAPABILITIES.iter().copied()
        .filter(|capability| !adapter.supports(*capability)).collect::<Vec<_>>();
    if missing.is_empty() { Ok(()) } else { Err(missing) }
}
