pub use sea_orm_migration::prelude::*;

mod m20260716_000001_init;
mod m20260716_000002_site_icons;
mod m20260716_000003_ai_digest;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260716_000001_init::Migration),
            Box::new(m20260716_000002_site_icons::Migration),
            Box::new(m20260716_000003_ai_digest::Migration),
        ]
    }
}
