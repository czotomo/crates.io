//! Endpoint for searching and discovery functionality

use crate::auth::AuthCheck;
use diesel::dsl::*;
use diesel::sql_types::{Array, Bool, Text};
use diesel_full_text_search::*;
use once_cell::sync::OnceCell;

use crate::controllers::cargo_prelude::*;
use crate::controllers::helpers::Paginate;
use crate::models::{Crate, CrateOwner, CrateVersions, OwnerKind, TopVersions, Version};
use crate::schema::*;
use crate::util::errors::bad_request;
use crate::views::EncodableCrate;

use crate::controllers::helpers::pagination::{Page, Paginated, PaginationOptions};
use crate::models::krate::ALL_COLUMNS;
use crate::sql::{array_agg, canon_crate_name, lower};

/// Handles the `GET /crates` route.
/// Returns a list of crates. Called in a variety of scenarios in the
/// front end, including:
/// - Alphabetical listing of crates
/// - List of crates under a specific owner
/// - Listing a user's followed crates
///
/// Notes:
/// The different use cases this function covers is handled through passing
/// in parameters in the GET request.
///
/// We would like to stop adding functionality in here. It was built like
/// this to keep the number of database queries low, though given Rust's
/// low performance overhead, this is a soft goal to have, and can afford
/// more database transactions if it aids understandability.
///
/// All of the edge cases for this function are not currently covered
/// in testing, and if they fail, it is difficult to determine what
/// caused the break. In the future, we should look at splitting this
/// function out to cover the different use cases, and create unit tests
/// for them.
pub async fn search(app: AppState, req: Parts) -> AppResult<Json<Value>> {
    spawn_blocking(move || {
        use diesel::sql_types::Float;
        use seek::*;

        let params = req.query();
        let option_param = |s| params.get(s).map(|v| v.as_str());
        let sort = option_param("sort");
        let include_yanked = option_param("include_yanked")
            .map(|s| s == "yes")
            .unwrap_or(true);

        // Remove 0x00 characters from the query string because Postgres can not
        // handle them and will return an error, which would cause us to throw
        // an Internal Server Error ourselves.
        let q_string = option_param("q").map(|q| q.replace('\u{0}', ""));

        let filter_params = FilterParams {
            q_string: q_string.as_deref(),
            include_yanked,
            category: option_param("category"),
            all_keywords: option_param("all_keywords"),
            keyword: option_param("keyword"),
            letter: option_param("letter"),
            user_id: option_param("user_id").and_then(|s| s.parse::<i32>().ok()),
            team_id: option_param("team_id").and_then(|s| s.parse::<i32>().ok()),
            following: option_param("following").is_some(),
            has_ids: option_param("ids[]").is_some(),
            ..Default::default()
        };

        let selection = (
            ALL_COLUMNS,
            false.into_sql::<Bool>(),
            recent_crate_downloads::downloads.nullable(),
            0_f32.into_sql::<Float>(),
        );

        let conn = &mut *app.db_read()?;
        let mut seek: Option<Seek> = None;
        let mut query = filter_params
            .make_query(&req, conn)?
            .left_join(recent_crate_downloads::table)
            .select(selection);

        if let Some(q_string) = &q_string {
            if !q_string.is_empty() {
                let sort = sort.unwrap_or("relevance");

                query = query.order(Crate::with_name(q_string).desc());

                if sort == "relevance" {
                    let q = sql::<TsQuery>("plainto_tsquery('english', ")
                        .bind::<Text, _>(q_string)
                        .sql(")");
                    let rank = ts_rank_cd(crates::textsearchable_index_col, q);
                    query = query.select((
                        ALL_COLUMNS,
                        Crate::with_name(q_string),
                        recent_crate_downloads::downloads.nullable(),
                        rank.clone(),
                    ));
                    seek = Some(Seek::Relevance);
                    query = query.then_order_by(rank.desc())
                } else {
                    query = query.select((
                        ALL_COLUMNS,
                        Crate::with_name(q_string),
                        recent_crate_downloads::downloads.nullable(),
                        0_f32.into_sql::<Float>(),
                    ));
                    seek = Some(Seek::Query);
                }
            }
        }

        // Any sort other than 'relevance' (default) would ignore exact crate name matches
        // Seek-based pagination requires a unique ordering to avoid unexpected row skipping
        // during pagination.
        // Therefore, when the ordering isn't unique an auxiliary ordering column should be added
        // to ensure predictable pagination behavior.
        if sort == Some("downloads") {
            seek = Some(Seek::Downloads);
            query = query.order((crates::downloads.desc(), crates::id.desc()))
        } else if sort == Some("recent-downloads") {
            seek = Some(Seek::RecentDownloads);
            query = query.order((
                recent_crate_downloads::downloads.desc().nulls_last(),
                crates::id.desc(),
            ))
        } else if sort == Some("recent-updates") {
            seek = Some(Seek::RecentUpdates);
            query = query.order((crates::updated_at.desc(), crates::id.desc()));
        } else if sort == Some("new") {
            seek = Some(Seek::New);
            query = query.order((crates::created_at.desc(), crates::id.desc()));
        } else {
            seek = seek.or(Some(Seek::Name));
            // Since the name is unique value, the inherent ordering becomes naturally unique.
            // Therefore, an additional auxiliary ordering column is unnecessary in this case.
            query = query.then_order_by(crates::name.asc())
        }

        let pagination: PaginationOptions = PaginationOptions::builder()
            .limit_page_numbers()
            .enable_seek(true)
            .gather(&req)?;

        let explicit_page = matches!(pagination.page, Page::Numeric(_));

        // To avoid breaking existing users, seek-based pagination is only used if an explicit page has
        // not been provided. This way clients relying on meta.next_page will use the faster seek-based
        // paginations, while client hardcoding pages handling will use the slower offset-based code.
        let (total, next_page, prev_page, data, conn) = if !explicit_page && seek.is_some() {
            let seek = seek.unwrap();
            if let Some(condition) = seek
                .after(&pagination.page)?
                .map(|s| filter_params.seek_after(&s))
            {
                query = query.filter(condition);
            }

            // This does a full index-only scan over the crates table to gather how many crates were
            // published. Unfortunately on PostgreSQL counting the rows in a table requires scanning
            // the table, and the `total` field is part of the stable registries API.
            //
            // If this becomes a problem in the future the crates count could be denormalized, at least
            // for the filterless happy path.
            let query = query.pages_pagination_with_count_query(
                pagination,
                filter_params.make_query(&req, conn)?.count(),
            );
            let data: Paginated<(Crate, bool, Option<i64>, f32)> =
                info_span!("db.query", message = "SELECT ..., COUNT(*) FROM crates")
                    .in_scope(|| query.load(conn))?;

            (
                data.total(),
                data.next_seek_params(|last| seek.to_payload(last))?
                    .map(|p| req.query_with_params(p)),
                None,
                data.into_iter().collect::<Vec<_>>(),
                conn,
            )
        } else {
            let query = query.pages_pagination_with_count_query(
                pagination,
                filter_params.make_query(&req, conn)?.count(),
            );
            let data: Paginated<(Crate, bool, Option<i64>, f32)> =
                info_span!("db.query", message = "SELECT ..., COUNT(*) FROM crates")
                    .in_scope(|| query.load(conn))?;
            (
                data.total(),
                data.next_page_params().map(|p| req.query_with_params(p)),
                data.prev_page_params().map(|p| req.query_with_params(p)),
                data.into_iter().collect::<Vec<_>>(),
                conn,
            )
        };

        let perfect_matches = data.iter().map(|&(_, b, _, _)| b).collect::<Vec<_>>();
        let recent_downloads = data
            .iter()
            .map(|&(_, _, s, _)| s.unwrap_or(0))
            .collect::<Vec<_>>();
        let crates = data.into_iter().map(|(c, _, _, _)| c).collect::<Vec<_>>();

        let versions: Vec<Version> = info_span!("db.query", message = "SELECT ... FROM versions")
            .in_scope(|| crates.versions().load(conn))?;
        let versions = versions
            .grouped_by(&crates)
            .into_iter()
            .map(TopVersions::from_versions);

        let crates = versions
            .zip(crates)
            .zip(perfect_matches)
            .zip(recent_downloads)
            .map(
                |(((max_version, krate), perfect_match), recent_downloads)| {
                    EncodableCrate::from_minimal(
                        krate,
                        Some(&max_version),
                        Some(vec![]),
                        perfect_match,
                        Some(recent_downloads),
                    )
                },
            )
            .collect::<Vec<_>>();

        Ok(Json(json!({
            "crates": crates,
            "meta": {
                "total": total,
                "next_page": next_page,
                "prev_page": prev_page,
            },
        })))
    })
    .await
}

#[derive(Default)]
struct FilterParams<'a> {
    q_string: Option<&'a str>,
    include_yanked: bool,
    category: Option<&'a str>,
    all_keywords: Option<&'a str>,
    keyword: Option<&'a str>,
    letter: Option<&'a str>,
    user_id: Option<i32>,
    team_id: Option<i32>,
    following: bool,
    has_ids: bool,
    _auth_user_id: OnceCell<i32>,
    _ids: OnceCell<Option<Vec<String>>>,
}

impl<'a> FilterParams<'a> {
    fn ids(&self, req: &Parts) -> Option<&[String]> {
        self._ids
            .get_or_init(|| {
                if self.has_ids {
                    let query_bytes = req.uri.query().unwrap_or("").as_bytes();
                    let v = url::form_urlencoded::parse(query_bytes)
                        .filter(|(key, _)| key == "ids[]")
                        .map(|(_, value)| value.to_string())
                        .collect::<Vec<_>>();
                    Some(v)
                } else {
                    None
                }
            })
            .as_deref()
    }

    fn authed_user_id(&self, req: &Parts, conn: &mut PgConnection) -> AppResult<&i32> {
        self._auth_user_id.get_or_try_init(|| {
            let user_id = AuthCheck::default().check(req, conn)?.user_id();
            Ok(user_id)
        })
    }

    fn make_query(
        &'a self,
        req: &Parts,
        conn: &mut PgConnection,
    ) -> AppResult<crates::BoxedQuery<'a, diesel::pg::Pg>> {
        let mut query = crates::table.into_boxed();

        if let Some(q_string) = self.q_string {
            if !q_string.is_empty() {
                let q = sql::<TsQuery>("plainto_tsquery('english', ")
                    .bind::<Text, _>(q_string)
                    .sql(")");
                query = query.filter(
                    q.matches(crates::textsearchable_index_col)
                        .or(Crate::loosly_matches_name(q_string)),
                );
            }
        }

        if let Some(cat) = self.category {
            query = query.filter(
                crates::id.eq_any(
                    crates_categories::table
                        .select(crates_categories::crate_id)
                        .inner_join(categories::table)
                        .filter(
                            categories::slug
                                .eq(cat)
                                .or(categories::slug.like(format!("{cat}::%"))),
                        ),
                ),
            );
        }

        if let Some(kws) = self.all_keywords {
            let names: Vec<_> = kws
                .split_whitespace()
                .map(|name| name.to_lowercase())
                .collect();

            query = query.filter(
                // FIXME: Just use `.contains` in Diesel 2.0
                // https://github.com/diesel-rs/diesel/issues/2066
                Contains::new(
                    crates_keywords::table
                        .inner_join(keywords::table)
                        .filter(crates_keywords::crate_id.eq(crates::id))
                        .select(array_agg(keywords::keyword))
                        .single_value(),
                    names.into_sql::<Array<Text>>(),
                ),
            );
        } else if let Some(kw) = self.keyword {
            query = query.filter(
                crates::id.eq_any(
                    crates_keywords::table
                        .select(crates_keywords::crate_id)
                        .inner_join(keywords::table)
                        .filter(lower(keywords::keyword).eq(lower(kw))),
                ),
            );
        } else if let Some(letter) = self.letter {
            let pattern = format!(
                "{}%",
                letter
                    .chars()
                    .next()
                    .ok_or_else(|| bad_request("letter value must contain 1 character"))?
                    .to_lowercase()
                    .collect::<String>()
            );
            query = query.filter(canon_crate_name(crates::name).like(pattern));
        } else if let Some(user_id) = self.user_id {
            query = query.filter(
                crates::id.eq_any(
                    CrateOwner::by_owner_kind(OwnerKind::User)
                        .select(crate_owners::crate_id)
                        .filter(crate_owners::owner_id.eq(user_id)),
                ),
            );
        } else if let Some(team_id) = self.team_id {
            query = query.filter(
                crates::id.eq_any(
                    CrateOwner::by_owner_kind(OwnerKind::Team)
                        .select(crate_owners::crate_id)
                        .filter(crate_owners::owner_id.eq(team_id)),
                ),
            );
        } else if self.following {
            let user_id = self.authed_user_id(req, conn)?;
            query = query.filter(
                crates::id.eq_any(
                    follows::table
                        .select(follows::crate_id)
                        .filter(follows::user_id.eq(user_id)),
                ),
            );
        } else if self.ids(req).is_some() {
            query = query.filter(crates::name.eq_any(self.ids(req).unwrap()));
        }

        if !self.include_yanked {
            query = query.filter(exists(
                versions::table
                    .filter(versions::crate_id.eq(crates::id))
                    .filter(versions::yanked.eq(false)),
            ));
        }

        Ok(query)
    }

    fn seek_after(&self, seek_payload: &seek::SeekPayload) -> BoxedCondition<'a> {
        use seek::*;

        let crates_aliased = alias!(crates as crates_aliased);
        let crate_name_by_id = |id: i32| {
            crates_aliased
                .find(id)
                .select(crates_aliased.field(crates::name))
                .single_value()
        };
        let conditions: Vec<BoxedCondition<'_>> = match *seek_payload {
            SeekPayload::Name(Name(id)) => {
                // Equivalent of:
                // `WHERE name > name'`
                vec![Box::new(crates::name.nullable().gt(crate_name_by_id(id)))]
            }
            SeekPayload::New(New(created_at, id)) => {
                // Equivalent of:
                // `WHERE (created_at = created_at' AND id < id') OR created_at < created_at'`
                vec![
                    Box::new(
                        crates::created_at
                            .eq(created_at)
                            .and(crates::id.lt(id))
                            .nullable(),
                    ),
                    Box::new(crates::created_at.lt(created_at).nullable()),
                ]
            }
            SeekPayload::RecentUpdates(RecentUpdates(updated_at, id)) => {
                // Equivalent of:
                // `WHERE (updated_at = updated_at' AND id < id') OR updated_at < updated_at'`
                vec![
                    Box::new(
                        crates::updated_at
                            .eq(updated_at)
                            .and(crates::id.lt(id))
                            .nullable(),
                    ),
                    Box::new(crates::updated_at.lt(updated_at).nullable()),
                ]
            }
            SeekPayload::RecentDownloads(RecentDownloads(recent_downloads, id)) => {
                // Equivalent of:
                // for recent_downloads is not None:
                // `WHERE (recent_downloads = recent_downloads' AND id < id')
                //      OR (recent_downloads < recent_downloads' OR recent_downloads IS NULL)`
                // for recent_downloads is None:
                // `WHERE (recent_downloads IS NULL AND id < id')`
                match recent_downloads {
                    Some(dl) => {
                        vec![
                            Box::new(
                                recent_crate_downloads::downloads
                                    .eq(dl)
                                    .and(crates::id.lt(id))
                                    .nullable(),
                            ),
                            Box::new(
                                recent_crate_downloads::downloads
                                    .lt(dl)
                                    .or(recent_crate_downloads::downloads.is_null())
                                    .nullable(),
                            ),
                        ]
                    }
                    None => {
                        vec![Box::new(
                            recent_crate_downloads::downloads
                                .is_null()
                                .and(crates::id.lt(id))
                                .nullable(),
                        )]
                    }
                }
            }
            SeekPayload::Downloads(Downloads(downloads, id)) => {
                // Equivalent of:
                // `WHERE (downloads = downloads' AND id < id') OR downloads < downloads'`
                vec![
                    Box::new(
                        crates::downloads
                            .eq(downloads)
                            .and(crates::id.lt(id))
                            .nullable(),
                    ),
                    Box::new(crates::downloads.lt(downloads).nullable()),
                ]
            }
            SeekPayload::Query(Query(exact_match, id)) => {
                // Equivalent of:
                // `WHERE (exact_match = exact_match' AND name < name') OR exact_match <
                // exact_match'`
                let q_string = self.q_string.expect("q_string should not be None");
                let name_exact_match = Crate::with_name(q_string);
                vec![
                    Box::new(
                        name_exact_match
                            .eq(exact_match)
                            .and(crates::name.nullable().gt(crate_name_by_id(id)))
                            .nullable(),
                    ),
                    Box::new(name_exact_match.lt(exact_match).nullable()),
                ]
            }
            SeekPayload::Relevance(Relevance(exact, rank_in, id)) => {
                // Equivalent of:
                // `WHERE (exact_match = exact_match' AND rank = rank' AND name > name')
                //      OR (exact_match = exact_match' AND rank < rank')
                //      OR exact_match < exact_match'`
                let q_string = self.q_string.expect("q_string should not be None");
                let q = to_tsquery_with_search_config(
                    configuration::TsConfigurationByName("english"),
                    q_string,
                );
                let rank = ts_rank_cd(crates::textsearchable_index_col, q);
                let name_exact_match = Crate::with_name(q_string);
                vec![
                    Box::new(
                        name_exact_match
                            .eq(exact)
                            .and(rank.eq(rank_in))
                            .and(crates::name.nullable().gt(crate_name_by_id(id)))
                            .nullable(),
                    ),
                    Box::new(name_exact_match.eq(exact).and(rank.lt(rank_in)).nullable()),
                    Box::new(name_exact_match.lt(exact).nullable()),
                ]
            }
        };

        conditions
            .into_iter()
            .fold(
                None,
                |merged_condition: Option<BoxedCondition<'_>>, condition| {
                    Some(match merged_condition {
                        Some(merged) => Box::new(merged.or(condition)),
                        None => condition,
                    })
                },
            )
            .expect("should be a reduced BoxedCondition")
    }
}

mod seek {
    use crate::controllers::helpers::pagination::seek;
    use crate::models::Crate;
    use chrono::naive::serde::ts_microseconds;

    seek! {
        pub enum Seek {
            Name(i32)
            New(#[serde(with="ts_microseconds")] chrono::NaiveDateTime, i32)
            RecentUpdates(#[serde(with="ts_microseconds")] chrono::NaiveDateTime, i32)
            RecentDownloads(Option<i64>, i32)
            Downloads(i32, i32)
            Query(bool, i32)
            Relevance(bool, f32, i32)
        }
    }

    impl Seek {
        pub(crate) fn to_payload(&self, record: &(Crate, bool, Option<i64>, f32)) -> SeekPayload {
            match *self {
                Seek::Name => SeekPayload::Name(Name(record.0.id)),
                Seek::New => SeekPayload::New(New(record.0.created_at, record.0.id)),
                Seek::RecentUpdates => {
                    SeekPayload::RecentUpdates(RecentUpdates(record.0.updated_at, record.0.id))
                }
                Seek::RecentDownloads => {
                    SeekPayload::RecentDownloads(RecentDownloads(record.2, record.0.id))
                }
                Seek::Downloads => {
                    SeekPayload::Downloads(Downloads(record.0.downloads, record.0.id))
                }
                Seek::Query => SeekPayload::Query(Query(record.1, record.0.id)),
                Seek::Relevance => {
                    SeekPayload::Relevance(Relevance(record.1, record.3, record.0.id))
                }
            }
        }
    }
}

type BoxedCondition<'a> = Box<
    dyn BoxableExpression<
            LeftJoinQuerySource<crates::table, recent_crate_downloads::table>,
            diesel::pg::Pg,
            SqlType = diesel::sql_types::Nullable<Bool>,
        > + 'a,
>;

diesel::infix_operator!(Contains, "@>");
