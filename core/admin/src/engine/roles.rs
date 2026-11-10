//! The operator behind a request: the audit label and the role every check compares.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use controlplane::Role;

use crate::{AdminState, Authed};

/// The label an `ADMIN_OPEN=1` request acts under.
pub(crate) const OPEN_OPERATOR: &str = "Local Admin";

#[derive(Clone, Debug)]
pub(crate) struct Operator {
    pub(crate) login: String,
    pub(crate) role: Role,
}

/// `ADMIN_OPEN=1` acts as `Admin`; otherwise the session user's `admin.users.role`.
pub(crate) async fn resolve(st: &AdminState, authed: &Authed) -> Result<Operator, Box<Response>> {
    if st.open {
        return Ok(Operator { login: OPEN_OPERATOR.to_string(), role: Role::Admin });
    }
    let row: Option<(String,)> = sqlx::query_as("SELECT role FROM admin.users WHERE username = $1")
        .bind(&authed.username)
        .fetch_optional(&st.pool)
        .await
        .map_err(|e| {
            tracing::error!(err = %e, "admin role lookup failed");
            Box::new((StatusCode::INTERNAL_SERVER_ERROR, "role lookup failed").into_response())
        })?;
    let Some((raw,)) = row else {
        return Err(Box::new((StatusCode::UNAUTHORIZED, "unauthorized").into_response()));
    };
    let role = crate::parse_role(&raw).map_err(|e| {
        tracing::error!(err = %e, user = %authed.username, "admin.users holds an unknown role");
        Box::new((StatusCode::INTERNAL_SERVER_ERROR, "role lookup failed").into_response())
    })?;
    Ok(Operator { login: authed.username.clone(), role })
}

pub(crate) fn allows(operator: &Operator, min_role: Role) -> bool {
    operator.role >= min_role
}
