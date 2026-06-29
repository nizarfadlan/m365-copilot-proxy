use utoipa::ToSchema;

#[derive(ToSchema, serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub token: TokenStatusResponse,
}

#[derive(ToSchema, serde::Serialize)]
pub struct TokenStatusResponse {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub seconds_remaining: i64,
}

#[derive(ToSchema, serde::Serialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelObject>,
}

#[derive(ToSchema, serde::Serialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub owned_by: String,
}

#[derive(ToSchema, serde::Serialize)]
pub struct ErrorResponse {
    pub detail: String,
}
