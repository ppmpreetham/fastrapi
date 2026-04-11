use std::sync::LazyLock;

pub struct Config {}

pub fn config() -> &'static Config {
    static CONFIG: LazyLock<Config> = LazyLock::new(|| {
        Config {
            // TODO: TBD
        }
    });
    &*CONFIG
}
