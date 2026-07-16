pub use sea_orm_migration::prelude::*;

mod m20260716_000001_init;
mod m20260716_000002_site_icons;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260716_000001_init::Migration),
            Box::new(m20260716_000002_site_icons::Migration),
        ]
    }
}
