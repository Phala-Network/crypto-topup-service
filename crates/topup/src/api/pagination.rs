//! Stripe's list parameters: cursor pagination (<https://docs.stripe.com/api/pagination>),
//! `limit` (1 to 100, default 10) and `starting_after` or `ending_before`, an object id of the
//! list; and the `created` range filters.

use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

use super::error::ApiError;
use crate::ids;

/// A `created[gt|gte|lt|lte]=<Unix seconds>` bound (<https://docs.stripe.com/api/events/list>) as a
/// SQL comparison of the stored timestamp, compared at whole seconds as the API shows `created`:
/// `gt` is at or after the next second and `lte` before it. `None` for another parameter.
pub(crate) fn created_bound(
    name: &str,
    value: &str,
) -> Result<Option<(&'static str, DateTime<Utc>)>, ApiError> {
    let (operator, next_second) = match name {
        "created[gt]" => (">=", true),
        "created[gte]" => (">=", false),
        "created[lt]" => ("<", false),
        "created[lte]" => ("<", true),
        _ => return Ok(None),
    };
    let bound = value
        .parse::<i64>()
        .ok()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .and_then(|at| {
            if next_second {
                at.checked_add_signed(TimeDelta::seconds(1))
            } else {
                Some(at)
            }
        })
        .ok_or_else(|| ApiError::invalid_param(name, format!("{name} must be Unix seconds")))?;
    Ok(Some((operator, bound)))
}

/// Default page size.
pub(crate) const DEFAULT_LIMIT: i64 = 10;
/// Largest page size.
pub(crate) const MAX_LIMIT: i64 = 100;

/// A page request: its size, and the object it starts after (or, with `before`, ends before).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Page {
    /// At most this many objects.
    pub(crate) limit: i64,
    /// The cursor object's id.
    pub(crate) cursor: Option<Uuid>,
    /// Whether `cursor` is `ending_before`.
    pub(crate) before: bool,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            cursor: None,
            before: false,
        }
    }
}

impl Page {
    /// Takes the query parameter `name` when it is a pagination parameter, with cursors that are
    /// ids of `prefix`; `Ok(false)` leaves it to the caller.
    pub(crate) fn accept(
        &mut self,
        name: &str,
        value: &str,
        prefix: &str,
    ) -> Result<bool, ApiError> {
        match name {
            "limit" => {
                self.limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            "starting_after" | "ending_before" => {
                let before = name == "ending_before";
                if self.cursor.is_some() && self.before != before {
                    return Err(ApiError::bad_request(
                        "starting_after and ending_before are mutually exclusive",
                    ));
                }
                self.cursor =
                    Some(ids::parse(prefix, value).ok_or_else(|| {
                        ApiError::invalid_param(name, format!("not a {prefix} id"))
                    })?);
                self.before = before;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The error for a cursor that names no object of the list.
    pub(crate) fn unknown_cursor(self, object: &str) -> ApiError {
        ApiError::invalid_param(
            if self.before {
                "ending_before"
            } else {
                "starting_after"
            },
            format!("no such {object}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_bounds_compare_whole_seconds() -> Result<(), ApiError> {
        let at = |seconds| DateTime::from_timestamp(seconds, 0);
        assert_eq!(
            created_bound("created[gt]", "10")?,
            Some((">=", at(11).unwrap_or_default()))
        );
        assert_eq!(
            created_bound("created[gte]", "10")?,
            Some((">=", at(10).unwrap_or_default()))
        );
        assert_eq!(
            created_bound("created[lt]", "10")?,
            Some(("<", at(10).unwrap_or_default()))
        );
        assert_eq!(
            created_bound("created[lte]", "10")?,
            Some(("<", at(11).unwrap_or_default()))
        );
        assert_eq!(created_bound("limit", "10")?, None);
        assert!(created_bound("created[gt]", "soon").is_err());
        Ok(())
    }

    #[test]
    fn pages_take_a_limit_and_one_cursor() -> Result<(), ApiError> {
        let id = ids::format(ids::TREASURY, Uuid::from_u128(7));
        let mut page = Page::default();
        assert!(page.accept("limit", "3", ids::TREASURY)?);
        assert!(page.accept("ending_before", &id, ids::TREASURY)?);
        assert!(!page.accept("status", "active", ids::TREASURY)?);
        assert_eq!(
            page,
            Page {
                limit: 3,
                cursor: Some(Uuid::from_u128(7)),
                before: true,
            }
        );
        assert!(page.accept("starting_after", &id, ids::TREASURY).is_err());
        assert!(
            Page::default()
                .accept("limit", "101", ids::TREASURY)
                .is_err()
        );
        assert!(
            Page::default()
                .accept("starting_after", "key_1", ids::TREASURY)
                .is_err()
        );
        Ok(())
    }
}
