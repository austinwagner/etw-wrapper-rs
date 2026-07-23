use etw::gen_etw_wrapper;

gen_etw_wrapper!("manifests/provider.man");

pub fn register() -> etw::Result<RenamedProviderLogger> {
    RenamedProviderLogger::register()
}
