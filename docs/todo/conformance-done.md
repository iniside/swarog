# Conformance

Last updated: 2026-11-24

- [x] **The cp.invoke request_id policy row does not list notifications** (2026-11-24, admin-second-pass step 14)
  `CONTROL_PLANE_INVOKE_REQUEST_ID_RATIONALE` (`tools/conformance/src/policy.rs`) now says
  notifications persists only the SHA-256 operator dedup key of the request id, a fixed
  shape `Service::send_operator_mail` checks before any statement.
