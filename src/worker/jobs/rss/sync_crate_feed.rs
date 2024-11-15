use crate::schema::{crates, versions};
use crate::storage::FeedId;
use crate::worker::Environment;
use chrono::Duration;
use crates_io_worker::BackgroundJob;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use std::sync::Arc;

/// Items younger than this will always be included in the feed.
const ALWAYS_INCLUDE_AGE: Duration = Duration::hours(24);

/// The number of items to include in the feed.
///
/// If there are less than this number of items in the database, the feed will
/// contain fewer items. If there are more items in the database that are
/// younger than [`ALWAYS_INCLUDE_AGE`], all of them will be included in
/// the feed.
const NUM_ITEMS: i64 = 10;

#[derive(Serialize, Deserialize)]
pub struct SyncCrateFeed {
    name: String,
}

impl SyncCrateFeed {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

impl BackgroundJob for SyncCrateFeed {
    const JOB_NAME: &'static str = "sync_crate_feed";
    const DEDUPLICATED: bool = true;

    type Context = Arc<Environment>;

    async fn run(&self, ctx: Self::Context) -> anyhow::Result<()> {
        let name = &self.name;
        let domain = &ctx.config.domain_name;

        info!("Loading latest {NUM_ITEMS} version updates for `{name}` from the database…");
        let mut conn = ctx.deadpool.get().await?;

        let version_updates = load_version_updates(name, &mut conn).await?;

        let feed_id = FeedId::Crate { name };

        let link = rss::extension::atom::Link {
            href: ctx.storage.feed_url(&feed_id),
            rel: "self".to_string(),
            mime_type: Some("application/rss+xml".to_string()),
            ..Default::default()
        };

        let items = version_updates
            .into_iter()
            .map(|u| u.into_rss_item(name, domain))
            .collect();

        let namespaces = vec![("crates".to_string(), "https://crates.io/".to_string())];
        let namespaces = namespaces.into_iter().collect();

        let channel = rss::Channel {
            title: format!("crates.io: {name} releases"),
            link: format!("https://{domain}/crates/{name}"),
            description: format!(
                "Recent releases of the {name} crate on the crates.io package registry"
            ),
            language: Some("en".to_string()),
            atom_ext: Some(rss::extension::atom::AtomExtension { links: vec![link] }),
            namespaces,
            items,
            ..Default::default()
        };

        info!("Uploading feed to storage…");
        ctx.storage.upload_feed(&feed_id, &channel).await?;

        let path = object_store::path::Path::from(&feed_id);
        if let Err(error) = ctx.invalidate_cdns(path.as_ref()).await {
            warn!("Failed to invalidate CDN caches: {error}");
        }

        info!("Finished syncing updates feed");
        Ok(())
    }
}

/// Load the latest versions from the database.
///
/// This function will load all versions from the database that are younger
/// than [`ALWAYS_INCLUDE_AGE`]. If there are less than [`NUM_ITEMS`] versions
/// then the list will be padded with older versions until [`NUM_ITEMS`] are
/// returned.
async fn load_version_updates(
    name: &str,
    conn: &mut AsyncPgConnection,
) -> QueryResult<Vec<VersionUpdate>> {
    let threshold_dt = chrono::Utc::now().naive_utc() - ALWAYS_INCLUDE_AGE;

    let updates = versions::table
        .inner_join(crates::table)
        .filter(crates::name.eq(name))
        .filter(versions::created_at.gt(threshold_dt))
        .order(versions::created_at.desc())
        .select(VersionUpdate::as_select())
        .load(conn)
        .await?;

    let num_updates = updates.len();
    if num_updates as i64 >= NUM_ITEMS {
        return Ok(updates);
    }

    versions::table
        .inner_join(crates::table)
        .filter(crates::name.eq(name))
        .order(versions::created_at.desc())
        .select(VersionUpdate::as_select())
        .limit(NUM_ITEMS)
        .load(conn)
        .await
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct VersionUpdate {
    #[diesel(select_expression = versions::columns::num)]
    version: String,
    #[diesel(select_expression = versions::columns::created_at)]
    time: chrono::NaiveDateTime,
}

impl VersionUpdate {
    fn into_rss_item(self, name: &str, domain: &str) -> rss::Item {
        let title = format!("New crate version published: {} v{}", name, self.version);
        let link = format!("https://{domain}/crates/{}/{}", name, self.version);
        let pub_date = self.time.and_utc().to_rfc2822();

        let guid = rss::Guid {
            value: link.clone(),
            permalink: true,
        };

        let name_extension = rss::extension::Extension {
            name: "crates:name".into(),
            value: Some(name.to_string()),
            ..Default::default()
        };

        let version_extension = rss::extension::Extension {
            name: "crates:version".into(),
            value: Some(self.version),
            ..Default::default()
        };

        let extensions = vec![
            ("name".to_string(), vec![name_extension]),
            ("version".to_string(), vec![version_extension]),
        ];
        let extensions = extensions.into_iter().collect();
        let extensions = vec![("crates".to_string(), extensions)];
        let extensions = extensions.into_iter().collect();

        rss::Item {
            guid: Some(guid),
            title: Some(title),
            link: Some(link),
            pub_date: Some(pub_date),
            extensions,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use crates_io_test_db::TestDatabase;
    use diesel_async::AsyncConnection;
    use futures_util::future::join_all;
    use insta::assert_debug_snapshot;
    use std::borrow::Cow;
    use std::future::Future;

    #[tokio::test]
    async fn test_load_version_updates() {
        crate::util::tracing::init_for_test();

        let db = TestDatabase::new();
        let mut conn = AsyncPgConnection::establish(db.url()).await.unwrap();

        let now = chrono::Utc::now().naive_utc();

        let updates = assert_ok!(load_version_updates("foo", &mut conn).await);
        assert_eq!(updates.len(), 0);

        let foo = create_crate(&mut conn, "foo").await;

        // If there are less than NUM_ITEMS versions, they should all be returned
        let futures = [
            create_version(&mut conn, foo, "1.0.0", now - Duration::days(123)),
            create_version(&mut conn, foo, "1.0.1", now - Duration::days(110)),
            create_version(&mut conn, foo, "1.1.0", now - Duration::days(100)),
            create_version(&mut conn, foo, "1.2.0", now - Duration::days(90)),
        ];
        join_all(futures).await;

        let updates = assert_ok!(load_version_updates("foo", &mut conn).await);
        assert_eq!(updates.len(), 4);
        assert_debug_snapshot!(updates.iter().map(|u| &u.version).collect::<Vec<_>>());

        // If there are more than NUM_ITEMS versions, only the most recent NUM_ITEMS should be returned
        let mut futures = Vec::new();
        for i in 1..=NUM_ITEMS {
            let version = format!("1.2.{i}");
            let publish_time = now - Duration::days(90) + Duration::hours(i);
            futures.push(create_version(&mut conn, foo, version, publish_time));
        }
        join_all(futures).await;

        let updates = assert_ok!(load_version_updates("foo", &mut conn).await);
        assert_eq!(updates.len() as i64, NUM_ITEMS);
        assert_debug_snapshot!(updates.iter().map(|u| &u.version).collect::<Vec<_>>());

        // But if there are more than NUM_ITEMS versions that are younger than ALWAYS_INCLUDE_AGE, all of them should be returned
        let mut futures = Vec::new();
        for i in 1..=(NUM_ITEMS + 10) {
            let version = format!("1.3.{i}");
            let publish_time = now - Duration::minutes(30) + Duration::seconds(i);
            futures.push(create_version(&mut conn, foo, version, publish_time));
        }
        join_all(futures).await;

        let updates = assert_ok!(load_version_updates("foo", &mut conn).await);
        assert_eq!(updates.len() as i64, NUM_ITEMS + 10);
        assert_debug_snapshot!(updates.iter().map(|u| &u.version).collect::<Vec<_>>());
    }

    async fn create_crate(conn: &mut AsyncPgConnection, name: &str) -> i32 {
        diesel::insert_into(crates::table)
            .values((crates::name.eq(name),))
            .returning(crates::id)
            .get_result(conn)
            .await
            .unwrap()
    }

    fn create_version(
        conn: &mut AsyncPgConnection,
        crate_id: i32,
        version: impl Into<Cow<'static, str>>,
        publish_time: NaiveDateTime,
    ) -> impl Future<Output = i32> {
        let version = version.into();
        let future = diesel::insert_into(versions::table)
            .values((
                versions::crate_id.eq(crate_id),
                versions::num.eq(version.clone()),
                versions::num_no_build.eq(version),
                versions::created_at.eq(publish_time),
                versions::updated_at.eq(publish_time),
                versions::checksum.eq("checksum"),
                versions::crate_size.eq(0),
            ))
            .returning(versions::id)
            .get_result(conn);

        async move { future.await.unwrap() }
    }
}
