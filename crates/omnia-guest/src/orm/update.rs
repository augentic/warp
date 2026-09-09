use std::marker::PhantomData;

use anyhow::Result;
use sea_query::{Alias, SimpleExpr, Value};

use super::entity::Entity;
use super::filter::Filter;
use super::query::{Query, finish};

/// Builder for constructing UPDATE queries.
pub struct UpdateBuilder<M: Entity> {
    set_clauses: Vec<(&'static str, Value)>,
    filters: Vec<SimpleExpr>,
    returning: Vec<&'static str>,
    _marker: PhantomData<M>,
}

impl<M: Entity> Default for UpdateBuilder<M> {
    fn default() -> Self {
        Self {
            set_clauses: Vec::new(),
            filters: Vec::new(),
            returning: Vec::new(),
            _marker: PhantomData,
        }
    }
}

impl<M: Entity> UpdateBuilder<M> {
    /// Creates a new UPDATE query builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a column to a new value.
    #[must_use]
    pub fn set<V>(mut self, column: &'static str, value: V) -> Self
    where
        V: Into<Value>,
    {
        self.set_clauses.push((column, value.into()));
        self
    }

    /// Adds a WHERE clause filter.
    #[must_use]
    pub fn r#where(mut self, filter: Filter) -> Self {
        self.filters.push(filter.into_expr(M::TABLE));
        self
    }

    /// Specifies columns to return from updated rows.
    #[must_use]
    pub fn returning(mut self, column: &'static str) -> Self {
        self.returning.push(column);
        self
    }

    /// Build the UPDATE query.
    ///
    /// # Errors
    ///
    /// Returns an error if no `SET` clause or no `WHERE` filter was set (an
    /// unfiltered UPDATE would rewrite every row), or if a query value cannot
    /// be converted to a WASI data type.
    pub fn build(self) -> Result<Query> {
        if self.set_clauses.is_empty() {
            anyhow::bail!("UPDATE has no `.set(...)` clause");
        }
        if self.filters.is_empty() {
            anyhow::bail!("refusing to build an unfiltered UPDATE; add a `.where(...)` clause");
        }

        let mut statement = sea_query::Query::update();
        statement.table(Alias::new(M::TABLE));

        for (column, value) in self.set_clauses {
            statement.value(Alias::new(column), value);
        }

        for expr in self.filters {
            statement.and_where(expr);
        }

        for column in self.returning {
            statement.returning_col(Alias::new(column));
        }

        finish(&statement, M::TABLE, "update")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::Stop;
    use super::*;
    use crate::orm::DataType;

    #[test]
    fn set_chain_with_filter() {
        let query = UpdateBuilder::<Stop>::new()
            .set("name", "Britomart")
            .set("stop_id", 9)
            .r#where(Filter::eq("stop_id", 7))
            .build()
            .unwrap();

        assert_eq!(
            query.sql,
            r#"UPDATE "stop" SET "name" = $1, "stop_id" = $2 WHERE ("stop"."stop_id") = ($3)"#
        );
        assert!(matches!(&query.params[0], DataType::Str(Some(s)) if s == "Britomart"));
        assert!(matches!(query.params[1], DataType::Int32(Some(9))));
        assert!(matches!(query.params[2], DataType::Int32(Some(7))));
    }

    #[test]
    fn conditional_sets() {
        // The optional-PATCH-field pattern: absent fields add no SET clause.
        let name: Option<&str> = Some("Britomart");
        let timezone: Option<&str> = None;

        let mut update = UpdateBuilder::<Stop>::new();
        if let Some(name) = name {
            update = update.set("name", name);
        }
        if let Some(timezone) = timezone {
            update = update.set("timezone", timezone);
        }
        let query = update.r#where(Filter::eq("stop_id", 7)).build().unwrap();

        assert_eq!(query.sql, r#"UPDATE "stop" SET "name" = $1 WHERE ("stop"."stop_id") = ($2)"#);
        assert_eq!(query.params.len(), 2);
    }

    #[test]
    fn missing_set_or_filter() {
        assert!(UpdateBuilder::<Stop>::new().r#where(Filter::eq("stop_id", 7)).build().is_err());
        assert!(UpdateBuilder::<Stop>::new().set("name", "x").build().is_err());
    }
}
