use super::*;

impl SqliteStore {
    /// Host-only administration; never expose through tenant transports.
    pub async fn deployment_revoke_token(
        &self,
        tenant: &TenantId,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let identity: String =
            sqlx::query_scalar("SELECT identity_id FROM tokens WHERE tenant_id = ? AND id = ?")
                .bind(tenant.as_str())
                .bind(token.as_str())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| request(ErrorCode::NotFound))?;
        sqlx::query("UPDATE tokens SET revoked_at_ms = COALESCE(revoked_at_ms, ?), updated_at_ms = ?, updated_by = ?, updated_by_kind = 'system' WHERE tenant_id = ? AND id = ?")
            .bind(now_ms).bind(now_ms).bind(identity).bind(tenant.as_str()).bind(token.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn deployment_disable_identity(
        &self,
        tenant: &TenantId,
        identity: &IdentityId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let result = sqlx::query("UPDATE identities SET disabled_at_ms = COALESCE(disabled_at_ms, ?), updated_at_ms = ?, updated_by = ?, updated_by_kind = 'system' WHERE tenant_id = ? AND id = ?")
            .bind(now_ms).bind(now_ms).bind(identity.as_str()).bind(tenant.as_str()).bind(identity.as_str()).execute(&self.writer).await?;
        if result.rows_affected() == 0 {
            return Err(request(ErrorCode::NotFound));
        }
        Ok(())
    }
}
