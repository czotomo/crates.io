use crate::builders::{CrateBuilder, VersionBuilder};
use crate::util::{RequestHelper, TestApp};
use crate::{new_category, new_user};
use crates_io::models::Category;
use crates_io::schema::crates;
use diesel::{dsl::*, prelude::*, update};
use googletest::prelude::*;
use http::StatusCode;
use insta::assert_json_snapshot;
use once_cell::sync::Lazy;
use regex::Regex;

#[test]
fn index() {
    let (app, anon) = TestApp::init().empty();
    for json in search_both(&anon, "") {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    let krate = app.db(|conn| {
        let u = new_user("foo")
            .create_or_update(None, &app.as_inner().emails, conn)
            .unwrap();
        CrateBuilder::new("fooindex", u.id).expect_build(conn)
    });

    for json in search_both(&anon, "") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
        assert_eq!(json.crates[0].name, krate.name);
        assert_eq!(json.crates[0].id, krate.name);
    }
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn index_queries() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    let (krate, krate2) = app.db(|conn| {
        let krate = CrateBuilder::new("foo_index_queries", user.id)
            .readme("readme")
            .description("description")
            .keyword("kw1")
            .expect_build(conn);

        let krate2 = CrateBuilder::new("BAR_INDEX_QUERIES", user.id)
            .keyword("KW1")
            .expect_build(conn);

        CrateBuilder::new("foo", user.id)
            .keyword("kw3")
            .expect_build(conn);

        CrateBuilder::new("two-keywords", user.id)
            .keyword("kw1")
            .keyword("kw3")
            .expect_build(conn);
        (krate, krate2)
    });

    for json in search_both(&anon, "q=baz") {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    // All of these fields should be indexed/searched by the queries
    for json in search_both(&anon, "q=foo") {
        assert_eq!(json.crates.len(), 2);
        assert_eq!(json.meta.total, 2);
    }

    for json in search_both(&anon, "q=kw1") {
        assert_eq!(json.crates.len(), 3);
        assert_eq!(json.meta.total, 3);
    }

    for json in search_both(&anon, "q=readme") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both(&anon, "q=description") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    // Query containing a space
    for json in search_both(&anon, "q=foo%20kw3") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both_by_user_id(&anon, user.id) {
        assert_eq!(json.crates.len(), 4);
        assert_eq!(json.meta.total, 4);
    }

    for json in search_both_by_user_id(&anon, 0) {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    for json in search_both(&anon, "letter=F") {
        assert_eq!(json.crates.len(), 2);
        assert_eq!(json.meta.total, 2);
    }

    for json in search_both(&anon, "letter=B") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both(&anon, "letter=b") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both(&anon, "letter=c") {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    for json in search_both(&anon, "keyword=kw1") {
        assert_eq!(json.crates.len(), 3);
        assert_eq!(json.meta.total, 3);
    }

    for json in search_both(&anon, "keyword=KW1") {
        assert_eq!(json.crates.len(), 3);
        assert_eq!(json.meta.total, 3);
    }

    for json in search_both(&anon, "keyword=kw2") {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    for json in search_both(&anon, "all_keywords=kw1%20kw3") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both(&anon, "q=foo&keyword=kw1") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }

    for json in search_both(&anon, "q=foo2&keyword=kw1") {
        assert_eq!(json.crates.len(), 0);
        assert_eq!(json.meta.total, 0);
    }

    app.db(|conn| {
        new_category("Category 1", "cat1", "Category 1 crates")
            .create_or_update(conn)
            .unwrap();
        new_category("Category 1::Ba'r", "cat1::bar", "Ba'r crates")
            .create_or_update(conn)
            .unwrap();
        Category::update_crate(conn, &krate, &["cat1"]).unwrap();
        Category::update_crate(conn, &krate2, &["cat1::bar"]).unwrap();
    });

    for cl in search_both(&anon, "category=cat1") {
        assert_eq!(cl.crates.len(), 2);
        assert_eq!(cl.meta.total, 2);
    }

    for cl in search_both(&anon, "category=cat1::bar") {
        assert_eq!(cl.crates.len(), 1);
        assert_eq!(cl.meta.total, 1);
    }

    for cl in search_both(&anon, "keyword=cat2") {
        assert_eq!(cl.crates.len(), 0);
        assert_eq!(cl.meta.total, 0);
    }

    for cl in search_both(&anon, "q=readme&category=cat1") {
        assert_eq!(cl.crates.len(), 1);
        assert_eq!(cl.meta.total, 1);
    }

    for cl in search_both(&anon, "keyword=kw1&category=cat1") {
        assert_eq!(cl.crates.len(), 2);
        assert_eq!(cl.meta.total, 2);
    }

    for cl in search_both(&anon, "keyword=kw3&category=cat1") {
        assert_eq!(cl.crates.len(), 0);
        assert_eq!(cl.meta.total, 0);
    }

    // ignores 0x00 characters that Postgres does not support
    for cl in search_both(&anon, "q=k%00w1") {
        assert_eq!(cl.meta.total, 3);
    }
}

#[test]
fn search_includes_crates_where_name_is_stopword() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();
    app.db(|conn| {
        CrateBuilder::new("which", user.id).expect_build(conn);
        CrateBuilder::new("should_be_excluded", user.id)
            .readme("crate which does things")
            .expect_build(conn);
    });
    for json in search_both(&anon, "q=which") {
        assert_eq!(json.crates.len(), 1);
        assert_eq!(json.meta.total, 1);
    }
}

#[test]
fn exact_match_first_on_queries() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("foo_exact", user.id)
            .description("bar_exact baz_exact")
            .expect_build(conn);

        CrateBuilder::new("bar-exact", user.id)
            .description("foo_exact baz_exact foo-exact baz_exact")
            .expect_build(conn);

        CrateBuilder::new("baz_exact", user.id)
            .description("foo-exact bar_exact foo-exact bar_exact foo_exact bar_exact")
            .expect_build(conn);

        CrateBuilder::new("other_exact", user.id)
            .description("other_exact")
            .expect_build(conn);
    });

    for json in search_both(&anon, "q=foo-exact") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "foo_exact");
        assert_eq!(json.crates[1].name, "baz_exact");
        assert_eq!(json.crates[2].name, "bar-exact");
    }

    for json in search_both(&anon, "q=bar_exact") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "bar-exact");
        assert_eq!(json.crates[1].name, "baz_exact");
        assert_eq!(json.crates[2].name, "foo_exact");
    }

    for json in search_both(&anon, "q=baz_exact") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "baz_exact");
        assert_eq!(json.crates[1].name, "bar-exact");
        assert_eq!(json.crates[2].name, "foo_exact");
    }
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn index_sorting() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    // To test that the unique ordering of seed-based pagination is correct, we need to
    // set some columns to the same value.
    app.db(|conn| {
        let krate1 = CrateBuilder::new("foo_sort", user.id)
            .description("bar_sort baz_sort const")
            .downloads(50)
            .recent_downloads(50)
            .expect_build(conn);

        let krate2 = CrateBuilder::new("bar_sort", user.id)
            .description("foo_sort baz_sort foo_sort baz_sort const")
            .downloads(3333)
            .recent_downloads(0)
            .expect_build(conn);

        let krate3 = CrateBuilder::new("baz_sort", user.id)
            .description("foo_sort bar_sort foo_sort bar_sort foo_sort bar_sort const")
            .downloads(100_000)
            .recent_downloads(50)
            .expect_build(conn);

        let krate4 = CrateBuilder::new("other_sort", user.id)
            .description("other_sort const")
            .downloads(100_000)
            .expect_build(conn);

        // Set the created at column for each crate
        update(&krate1)
            .set(crates::created_at.eq(now - 4.weeks()))
            .execute(conn)
            .unwrap();
        update(&krate2)
            .set(crates::created_at.eq(now - 1.weeks()))
            .execute(conn)
            .unwrap();
        update(crates::table.filter(crates::id.eq_any(vec![krate3.id, krate4.id])))
            .set(crates::created_at.eq(now - 3.weeks()))
            .execute(conn)
            .unwrap();

        // Set the updated at column for each crate
        update(&krate1)
            .set(crates::updated_at.eq(now - 3.weeks()))
            .execute(conn)
            .unwrap();
        update(crates::table.filter(crates::id.eq_any(vec![krate2.id, krate3.id])))
            .set(crates::updated_at.eq(now - 5.days()))
            .execute(conn)
            .unwrap();
        update(&krate4)
            .set(crates::updated_at.eq(now))
            .execute(conn)
            .unwrap();
    });

    // Sort by downloads
    for json in search_both(&anon, "sort=downloads") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "other_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "foo_sort");
    }
    let (resp, calls) = page_with_seek(&anon, "sort=downloads");
    assert_eq!(resp[0].crates[0].name, "other_sort");
    assert_eq!(resp[1].crates[0].name, "baz_sort");
    assert_eq!(resp[2].crates[0].name, "bar_sort");
    assert_eq!(resp[3].crates[0].name, "foo_sort");
    assert_eq!(resp[3].meta.total, 4);
    assert_eq!(calls, 5);

    // Sort by recent-downloads
    for json in search_both(&anon, "sort=recent-downloads") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "foo_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "other_sort");
    }
    let (resp, calls) = page_with_seek(&anon, "sort=recent-downloads");
    assert_eq!(resp[0].crates[0].name, "baz_sort");
    assert_eq!(resp[1].crates[0].name, "foo_sort");
    assert_eq!(resp[2].crates[0].name, "bar_sort");
    assert_eq!(resp[3].crates[0].name, "other_sort");
    assert_eq!(resp[3].meta.total, 4);
    assert_eq!(calls, 5);

    // Sort by recent-updates
    for json in search_both(&anon, "sort=recent-updates") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "other_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "foo_sort");
    }
    let (resp, calls) = page_with_seek(&anon, "sort=recent-updates");
    assert_eq!(resp[0].crates[0].name, "other_sort");
    assert_eq!(resp[1].crates[0].name, "baz_sort");
    assert_eq!(resp[2].crates[0].name, "bar_sort");
    assert_eq!(resp[3].crates[0].name, "foo_sort");
    assert_eq!(resp[3].meta.total, 4);
    assert_eq!(calls, 5);

    // Sort by new
    for json in search_both(&anon, "sort=new") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "bar_sort");
        assert_eq!(json.crates[1].name, "other_sort");
        assert_eq!(json.crates[2].name, "baz_sort");
        assert_eq!(json.crates[3].name, "foo_sort");
    }
    let (resp, calls) = page_with_seek(&anon, "sort=new");
    assert_eq!(resp[0].crates[0].name, "bar_sort");
    assert_eq!(resp[1].crates[0].name, "other_sort");
    assert_eq!(resp[2].crates[0].name, "baz_sort");
    assert_eq!(resp[3].crates[0].name, "foo_sort");
    assert_eq!(resp[3].meta.total, 4);
    assert_eq!(calls, 5);

    use std::cmp::Reverse;

    fn decode_seek<D: for<'a> serde::Deserialize<'a>>(seek: &str) -> anyhow::Result<D> {
        use base64::{engine::general_purpose, Engine};
        let decoded = serde_json::from_slice(&general_purpose::URL_SAFE_NO_PAD.decode(seek)?)?;
        Ok(decoded)
    }

    // Sort by alpha with query
    for query in ["sort=alpha&q=bar_sort", "sort=alpha&q=sort"] {
        let (resp, calls) = page_with_seek(&anon, query);
        assert_eq!(calls, resp[0].meta.total + 1);
        let decoded_seeks = resp
            .iter()
            .filter_map(|cl| {
                cl.meta
                    .next_page
                    .as_ref()
                    .map(|next_page| (next_page, cl.crates[0].name.to_owned()))
            })
            .filter_map(|(q, name)| {
                let query = url::form_urlencoded::parse(q.trim_start_matches('?').as_bytes())
                    .into_owned()
                    .collect::<indexmap::IndexMap<String, String>>();
                query.get("seek").map(|s| {
                    let d = decode_seek::<(bool, i32)>(s).unwrap();
                    (d.0, name)
                })
            })
            .collect::<Vec<_>>();
        // ordering (exact match desc, name asc)
        let mut sorted = decoded_seeks.to_vec();
        sorted.sort_by_key(|k| (Reverse(k.0), k.1.to_owned()));
        assert_eq!(sorted, decoded_seeks);
        for json in search_both(&anon, query) {
            assert_eq!(json.meta.total, resp[0].meta.total);
            for (c, r) in json.crates.iter().zip(&resp) {
                assert_eq!(c.name, r.crates[0].name);
            }
        }
    }

    // Sort by relevance
    for query in ["q=foo_sort", "q=sort"] {
        let (resp, calls) = page_with_seek(&anon, query);
        assert_eq!(calls, resp[0].meta.total + 1);
        let decoded_seeks = resp
            .iter()
            .filter_map(|cl| {
                cl.meta
                    .next_page
                    .as_ref()
                    .map(|next_page| (next_page, cl.crates[0].name.to_owned()))
            })
            .filter_map(|(q, name)| {
                let query = url::form_urlencoded::parse(q.trim_start_matches('?').as_bytes())
                    .into_owned()
                    .collect::<indexmap::IndexMap<String, String>>();
                query.get("seek").map(|s| {
                    let d = decode_seek::<(bool, f32, i32)>(s).unwrap();
                    (d.0, (d.1 * 1e12) as i64, name)
                })
            })
            .collect::<Vec<_>>();
        // ordering (exact match desc, rank desc, name asc)
        let mut sorted = decoded_seeks.clone();
        sorted.sort_by_key(|k| (Reverse(k.0), Reverse(k.1), k.2.to_owned()));
        assert_eq!(sorted, decoded_seeks);
        for json in search_both(&anon, query) {
            assert_eq!(json.meta.total, resp[0].meta.total);
            for (c, r) in json.crates.iter().zip(&resp) {
                assert_eq!(c.name, r.crates[0].name);
            }
        }
    }

    // Test for bug with showing null results first when sorting
    // by descending downloads
    for json in search_both(&anon, "sort=recent-downloads") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "foo_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "other_sort");
    }
    let (resp, calls) = page_with_seek(&anon, "sort=recent-downloads");
    assert_eq!(resp[0].crates[0].name, "baz_sort");
    assert_eq!(resp[1].crates[0].name, "foo_sort");
    assert_eq!(resp[2].crates[0].name, "bar_sort");
    assert_eq!(resp[3].crates[0].name, "other_sort");
    assert_eq!(resp[3].meta.total, 4);
    assert_eq!(calls, 5);
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn ignore_exact_match_on_queries_with_sort() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        let krate1 = CrateBuilder::new("foo_sort", user.id)
            .description("bar_sort baz_sort const")
            .downloads(50)
            .recent_downloads(50)
            .expect_build(conn);

        let krate2 = CrateBuilder::new("bar_sort", user.id)
            .description("foo_sort baz_sort foo_sort baz_sort const")
            .downloads(3333)
            .recent_downloads(0)
            .expect_build(conn);

        let krate3 = CrateBuilder::new("baz_sort", user.id)
            .description("foo_sort bar_sort foo_sort bar_sort foo_sort bar_sort const")
            .downloads(100_000)
            .recent_downloads(10)
            .expect_build(conn);

        let krate4 = CrateBuilder::new("other_sort", user.id)
            .description("other_sort const")
            .downloads(999_999)
            .expect_build(conn);

        // Set the created at column for each crate
        update(&krate1)
            .set(crates::created_at.eq(now - 4.weeks()))
            .execute(conn)
            .unwrap();
        update(&krate2)
            .set(crates::created_at.eq(now - 1.weeks()))
            .execute(conn)
            .unwrap();
        update(&krate3)
            .set(crates::created_at.eq(now - 2.weeks()))
            .execute(conn)
            .unwrap();
        update(&krate4)
            .set(crates::created_at.eq(now - 3.weeks()))
            .execute(conn)
            .unwrap();

        // Set the updated at column for each crate
        update(&krate1)
            .set(crates::updated_at.eq(now - 3.weeks()))
            .execute(conn)
            .unwrap();
        update(&krate2)
            .set(crates::updated_at.eq(now - 5.days()))
            .execute(conn)
            .unwrap();
        update(&krate3)
            .set(crates::updated_at.eq(now - 10.seconds()))
            .execute(conn)
            .unwrap();
        update(&krate4)
            .set(crates::updated_at.eq(now))
            .execute(conn)
            .unwrap();
    });

    // Sort by downloads, order always the same no matter the crate name query
    for json in search_both(&anon, "q=foo_sort&sort=downloads") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "bar_sort");
        assert_eq!(json.crates[2].name, "foo_sort");
    }

    for json in search_both(&anon, "q=bar_sort&sort=downloads") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "bar_sort");
        assert_eq!(json.crates[2].name, "foo_sort");
    }

    for json in search_both(&anon, "q=baz_sort&sort=downloads") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "bar_sort");
        assert_eq!(json.crates[2].name, "foo_sort");
    }

    for json in search_both(&anon, "q=const&sort=downloads") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "other_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "foo_sort");
    }

    // Sort by recent-downloads, order always the same no matter the crate name query
    for json in search_both(&anon, "q=bar_sort&sort=recent-downloads") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "foo_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
    }

    // Test for bug with showing null results first when sorting
    // by descending downloads
    for json in search_both(&anon, "sort=recent-downloads") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "foo_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "bar_sort");
        assert_eq!(json.crates[3].name, "other_sort");
    }

    // Sort by recent-updates
    for json in search_both(&anon, "q=bar_sort&sort=recent-updates") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "baz_sort");
        assert_eq!(json.crates[1].name, "bar_sort");
        assert_eq!(json.crates[2].name, "foo_sort");
    }

    // Sort by new
    for json in search_both(&anon, "q=bar_sort&sort=new") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "bar_sort");
        assert_eq!(json.crates[1].name, "baz_sort");
        assert_eq!(json.crates[2].name, "foo_sort");
    }
}

#[test]
fn multiple_ids() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("foo", user.id).expect_build(conn);
        CrateBuilder::new("bar", user.id).expect_build(conn);
        CrateBuilder::new("baz", user.id).expect_build(conn);
        CrateBuilder::new("other", user.id).expect_build(conn);
    });

    for json in search_both(
        &anon,
        "ids%5B%5D=foo&ids%5B%5D=bar&ids%5B%5D=baz&ids%5B%5D=baz&ids%5B%5D=unknown",
    ) {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "bar");
        assert_eq!(json.crates[1].name, "baz");
        assert_eq!(json.crates[2].name, "foo");
    }
}

#[test]
fn loose_search_order() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    let ordered = app.db(|conn| {
        // exact match should be first
        let one = CrateBuilder::new("temp", user.id)
            .readme("readme")
            .description("description")
            .keyword("kw1")
            .expect_build(conn);
        // temp_udp should match second because of _
        let two = CrateBuilder::new("temp_utp", user.id)
            .readme("readme")
            .description("description")
            .keyword("kw1")
            .expect_build(conn);
        // evalrs should match 3rd because of readme
        let three = CrateBuilder::new("evalrs", user.id)
            .readme("evalrs_temp evalrs_temp evalrs_temp")
            .description("description")
            .keyword("kw1")
            .expect_build(conn);
        // tempfile should appear 4th
        let four = CrateBuilder::new("tempfile", user.id)
            .readme("readme")
            .description("description")
            .keyword("kw1")
            .expect_build(conn);
        vec![one, two, three, four]
    });
    for search_temp in search_both(&anon, "q=temp") {
        assert_eq!(search_temp.meta.total, 4);
        assert_eq!(search_temp.crates.len(), 4);
        for (lhs, rhs) in search_temp.crates.iter().zip(&ordered) {
            assert_eq!(lhs.name, rhs.name);
        }
    }

    for search_temp in search_both(&anon, "q=te") {
        assert_eq!(search_temp.meta.total, 3);
        assert_eq!(search_temp.crates.len(), 3);
    }
}

#[test]
fn index_include_yanked() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("unyanked", user.id)
            .version(VersionBuilder::new("1.0.0"))
            .version(VersionBuilder::new("2.0.0"))
            .expect_build(conn);

        CrateBuilder::new("newest_yanked", user.id)
            .version(VersionBuilder::new("1.0.0"))
            .version(VersionBuilder::new("2.0.0").yanked(true))
            .expect_build(conn);

        CrateBuilder::new("oldest_yanked", user.id)
            .version(VersionBuilder::new("1.0.0").yanked(true))
            .version(VersionBuilder::new("2.0.0"))
            .expect_build(conn);

        CrateBuilder::new("all_yanked", user.id)
            .version(VersionBuilder::new("1.0.0").yanked(true))
            .version(VersionBuilder::new("2.0.0").yanked(true))
            .expect_build(conn);
    });

    // Include fully yanked (all versions were yanked) crates
    for json in search_both(&anon, "include_yanked=yes&sort=alphabetical") {
        assert_eq!(json.meta.total, 4);
        assert_eq!(json.crates[0].name, "all_yanked");
        assert_eq!(json.crates[1].name, "newest_yanked");
        assert_eq!(json.crates[2].name, "oldest_yanked");
        assert_eq!(json.crates[3].name, "unyanked");
    }

    // Do not include fully yanked (all versions were yanked) crates
    for json in search_both(&anon, "include_yanked=no&sort=alphabetical") {
        assert_eq!(json.meta.total, 3);
        assert_eq!(json.crates[0].name, "newest_yanked");
        assert_eq!(json.crates[1].name, "oldest_yanked");
        assert_eq!(json.crates[2].name, "unyanked");
    }
}

#[test]
fn yanked_versions_are_not_considered_for_max_version() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("foo_yanked_version", user.id)
            .description("foo")
            .version("1.0.0")
            .version(VersionBuilder::new("1.1.0").yanked(true))
            .expect_build(conn);
    });

    for json in search_both(&anon, "q=foo") {
        assert_eq!(json.meta.total, 1);
        assert_eq!(json.crates[0].max_version, "1.0.0");
    }
}

#[test]
fn max_stable_version() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("foo", user.id)
            .description("foo")
            .version("0.3.0")
            .version("1.0.0")
            .version(VersionBuilder::new("1.1.0").yanked(true))
            .version("2.0.0-beta.1")
            .version("0.3.1")
            .expect_build(conn);
    });

    for json in search_both(&anon, "q=foo") {
        assert_eq!(json.meta.total, 1);
        assert_eq!(json.crates[0].max_stable_version, Some("1.0.0".to_string()));
    }
}

/// Given two crates, one with downloads less than 90 days ago, the
/// other with all downloads greater than 90 days ago, check that
/// the order returned is by recent downloads, descending. Check
/// also that recent download counts are returned in recent_downloads,
/// and total downloads counts are returned in downloads, and that
/// these numbers do not overlap.
#[test]
fn test_recent_download_count() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        // More than 90 days ago
        CrateBuilder::new("green_ball", user.id)
            .description("For fetching")
            .downloads(10)
            .recent_downloads(0)
            .expect_build(conn);

        CrateBuilder::new("sweet_potato_snack", user.id)
            .description("For when better than usual")
            .downloads(5)
            .recent_downloads(2)
            .expect_build(conn);
    });

    for json in search_both(&anon, "sort=recent-downloads") {
        assert_eq!(json.meta.total, 2);

        assert_eq!(json.crates[0].name, "sweet_potato_snack");
        assert_eq!(json.crates[1].name, "green_ball");

        assert_eq!(json.crates[0].recent_downloads, Some(2));
        assert_eq!(json.crates[0].downloads, 5);

        assert_eq!(json.crates[1].recent_downloads, Some(0));
        assert_eq!(json.crates[1].downloads, 10);
    }
}

/// Given one crate with zero downloads, check that the crate
/// still shows up in index results, but that it displays 0
/// for both recent downloads and downloads.
#[test]
fn test_zero_downloads() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        // More than 90 days ago
        CrateBuilder::new("green_ball", user.id)
            .description("For fetching")
            .downloads(0)
            .recent_downloads(0)
            .expect_build(conn);
    });

    for json in search_both(&anon, "sort=recent-downloads") {
        assert_eq!(json.meta.total, 1);
        assert_eq!(json.crates[0].name, "green_ball");
        assert_eq!(json.crates[0].recent_downloads, Some(0));
        assert_eq!(json.crates[0].downloads, 0);
    }
}

/// Given two crates, one with more all-time downloads, the other with
/// more downloads in the past 90 days, check that the index page for
/// categories and keywords is sorted by recent downloads by default.
#[test]
fn test_default_sort_recent() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    let (green_crate, potato_crate) = app.db(|conn| {
        // More than 90 days ago
        let green_crate = CrateBuilder::new("green_ball", user.id)
            .description("For fetching")
            .keyword("dog")
            .downloads(10)
            .recent_downloads(10)
            .expect_build(conn);

        let potato_crate = CrateBuilder::new("sweet_potato_snack", user.id)
            .description("For when better than usual")
            .keyword("dog")
            .downloads(20)
            .recent_downloads(0)
            .expect_build(conn);

        (green_crate, potato_crate)
    });

    // test that index for keywords is sorted by recent_downloads
    // by default
    for json in search_both(&anon, "keyword=dog") {
        assert_eq!(json.meta.total, 2);

        assert_eq!(json.crates[0].name, "green_ball");
        assert_eq!(json.crates[1].name, "sweet_potato_snack");

        assert_eq!(json.crates[0].recent_downloads, Some(10));
        assert_eq!(json.crates[0].downloads, 10);

        assert_eq!(json.crates[1].recent_downloads, Some(0));
        assert_eq!(json.crates[1].downloads, 20);
    }

    app.db(|conn| {
        new_category("Animal", "animal", "animal crates")
            .create_or_update(conn)
            .unwrap();
        Category::update_crate(conn, &green_crate, &["animal"]).unwrap();
        Category::update_crate(conn, &potato_crate, &["animal"]).unwrap();
    });

    // test that index for categories is sorted by recent_downloads
    // by default
    for json in search_both(&anon, "category=animal") {
        assert_eq!(json.meta.total, 2);

        assert_eq!(json.crates[0].name, "green_ball");
        assert_eq!(json.crates[1].name, "sweet_potato_snack");

        assert_eq!(json.crates[0].recent_downloads, Some(10));
        assert_eq!(json.crates[0].downloads, 10);

        assert_eq!(json.crates[1].recent_downloads, Some(0));
        assert_eq!(json.crates[1].downloads, 20);
    }
}

#[test]
fn pagination_links_included_if_applicable() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("pagination_links_1", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_2", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_3", user.id).expect_build(conn);
    });

    // This uses a filter (`page=n`) to disable seek-based pagination, as seek-based pagination
    // does not return page numbers.

    let page1 = anon.search("letter=p&page=1&per_page=1");
    let page2 = anon.search("letter=p&page=2&per_page=1");
    let page3 = anon.search("letter=p&page=3&per_page=1");
    let page4 = anon.search("letter=p&page=4&per_page=1");

    assert_eq!(
        Some("?letter=p&page=2&per_page=1".to_string()),
        page1.meta.next_page
    );
    assert_eq!(None, page1.meta.prev_page);
    assert_eq!(
        Some("?letter=p&page=3&per_page=1".to_string()),
        page2.meta.next_page
    );
    assert_eq!(
        Some("?letter=p&page=1&per_page=1".to_string()),
        page2.meta.prev_page
    );
    assert_eq!(None, page4.meta.next_page);
    assert_eq!(
        Some("?letter=p&page=2&per_page=1".to_string()),
        page3.meta.prev_page
    );
    assert!([page1.meta.total, page2.meta.total, page3.meta.total]
        .iter()
        .all(|w| *w == 3));
    assert_eq!(page4.meta.total, 0);
}

#[test]
fn seek_based_pagination() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("pagination_links_1", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_2", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_3", user.id).expect_build(conn);
    });

    let mut url = Some("?per_page=1".to_string());
    let mut results = Vec::new();
    let mut calls = 0;
    while let Some(current_url) = url.take() {
        let resp = anon.search(current_url.trim_start_matches('?'));
        calls += 1;

        results.append(
            &mut resp
                .crates
                .iter()
                .map(|res| res.name.clone())
                .collect::<Vec<_>>(),
        );

        if let Some(new_url) = resp.meta.next_page {
            assert_that!(resp.crates, len(eq(1)));
            url = Some(new_url);
            assert_eq!(resp.meta.total, 3);
        } else {
            assert_that!(resp.crates, empty());
            assert_eq!(resp.meta.total, 0);
        }

        assert_eq!(resp.meta.prev_page, None);
    }

    assert_eq!(calls, 4);
    assert_eq!(
        vec![
            "pagination_links_1",
            "pagination_links_2",
            "pagination_links_3"
        ],
        results
    );
}

#[test]
fn test_pages_work_even_with_seek_based_pagination() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("pagination_links_1", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_2", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_3", user.id).expect_build(conn);
    });

    // The next_page returned by the request is seek-based
    let first = anon.search("per_page=1");
    assert!(first.meta.next_page.unwrap().contains("seek="));
    assert_eq!(first.meta.total, 3);

    // Calling with page=2 will revert to offset-based pagination
    let second = anon.search("page=2&per_page=1");
    assert!(second.meta.next_page.unwrap().contains("page=3"));
    assert_eq!(second.meta.total, 3);
}

#[test]
fn invalid_seek_parameter() {
    let (_app, anon, _cookie) = TestApp::init().with_user();

    let response = anon.get::<()>("/api/v1/crates?seek=broken");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_json_snapshot!(response.json());
}

#[test]
fn pagination_parameters_only_accept_integers() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        CrateBuilder::new("pagination_links_1", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_2", user.id).expect_build(conn);
        CrateBuilder::new("pagination_links_3", user.id).expect_build(conn);
    });

    let response =
        anon.get_with_query::<()>("/api/v1/crates", "page=1&per_page=100%22%EF%BC%8Cexception");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json(),
        json!({ "errors": [{ "detail": "invalid digit found in string" }] })
    );

    let response =
        anon.get_with_query::<()>("/api/v1/crates", "page=100%22%EF%BC%8Cexception&per_page=1");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json(),
        json!({ "errors": [{ "detail": "invalid digit found in string" }] })
    );
}

#[test]
fn crates_by_user_id() {
    let (app, _, user) = TestApp::init().with_user();
    let id = user.as_model().id;
    app.db(|conn| {
        CrateBuilder::new("foo_my_packages", id).expect_build(conn);
    });

    for response in search_both_by_user_id(&user, id) {
        assert_eq!(response.crates.len(), 1);
        assert_eq!(response.meta.total, 1);
    }
}

#[test]
fn crates_by_user_id_not_including_deleted_owners() {
    let (app, anon, user) = TestApp::init().with_user();
    let user = user.as_model();

    app.db(|conn| {
        let krate = CrateBuilder::new("foo_my_packages", user.id).expect_build(conn);
        krate.owner_remove(conn, "foo").unwrap();
    });

    for response in search_both_by_user_id(&anon, user.id) {
        assert_eq!(response.crates.len(), 0);
        assert_eq!(response.meta.total, 0);
    }
}

static PAGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"((?:^page|&page|\?page)=\d+)").unwrap());

// search with both offset-based (prepend with `page=1` query) and seek-based pagination
fn search_both<U: RequestHelper>(anon: &U, query: &str) -> [crate::CrateList; 2] {
    if PAGE_RE.is_match(query) {
        panic!("url already contains page param");
    }
    let (offset, seek) = (anon.search(&format!("page=1&{query}")), anon.search(query));
    assert!(offset
        .meta
        .next_page
        .as_deref()
        .unwrap_or("page=2")
        .contains("page=2"));
    assert!(seek
        .meta
        .next_page
        .as_deref()
        .unwrap_or("seek=")
        .contains("seek="));
    [offset, seek]
}

fn search_both_by_user_id<U: RequestHelper>(anon: &U, id: i32) -> [crate::CrateList; 2] {
    let url = format!("user_id={id}");
    search_both(anon, &url)
}

fn page_with_seek<U: RequestHelper>(anon: &U, query: &str) -> (Vec<crate::CrateList>, i32) {
    let mut url = Some(format!("?per_page=1&{query}"));
    let mut results = Vec::new();
    let mut calls = 0;
    while let Some(current_url) = url.take() {
        let resp = anon.search(current_url.trim_start_matches('?'));
        calls += 1;
        if calls > 200 {
            panic!("potential infinite loop detected!")
        }

        if let Some(ref new_url) = resp.meta.next_page {
            assert!(new_url.contains("seek="));
            assert_that!(resp.crates, len(eq(1)));
            url = Some(new_url.to_owned());
            assert_ne!(resp.meta.total, 0);
        } else {
            assert_that!(resp.crates, empty());
            assert_eq!(resp.meta.total, 0);
        }
        results.push(resp);
    }
    (results, calls)
}
