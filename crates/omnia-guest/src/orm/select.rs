use std::marker::PhantomData;

use sea_query::{Alias, ColumnRef, Order, SimpleExpr};

use super::entity::Entity;
use super::filter::Filter;
use super::join::{Join, JoinSpec};
use super::query::{Query, finish};

/// Builder for constructing SELECT queries.
pub struct SelectBuilder<M: Entity> {
    filters: Vec<SimpleExpr>,
    limit: Option<u64>,
    offset: Option<u64>,
    order: Vec<(ColumnRef, Order)>,
    joins: Vec<JoinSpec>,
    _marker: PhantomData<M>,
}

impl<M: Entity> Default for SelectBuilder<M> {
    fn default() -> Self {
        let joins = M::joins().into_iter().map(|join| join.into_join_spec(M::TABLE)).collect();

        Self {
            filters: Vec::new(),
            limit: None,
            offset: None,
            order: Vec::new(),
            joins,
            _marker: PhantomData,
        }
    }
}

impl<M: Entity> SelectBuilder<M> {
    /// Creates a new SELECT query builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a WHERE clause filter.
    #[must_use]
    pub fn r#where(mut self, filter: Filter) -> Self {
        self.filters.push(filter.into_expr(M::TABLE));
        self
    }

    /// Sets the maximum number of rows to return.
    #[must_use]
    pub const fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the number of rows to skip.
    #[must_use]
    pub const fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Adds ascending ORDER BY clause.
    #[must_use]
    pub fn order_by(mut self, table: Option<&'static str>, column: &'static str) -> Self {
        let table = table.unwrap_or(M::TABLE);
        self.order.push((table_column(table, column), Order::Asc));
        self
    }

    /// Adds descending ORDER BY clause.
    #[must_use]
    pub fn order_by_desc(mut self, table: Option<&'static str>, column: &'static str) -> Self {
        let table = table.unwrap_or(M::TABLE);
        self.order.push((table_column(table, column), Order::Desc));
        self
    }

    /// Adds a JOIN clause to the query.
    #[must_use]
    pub fn join(mut self, join: Join) -> Self {
        self.joins.push(join.into_join_spec(M::TABLE));
        self
    }

    /// Build the SELECT query.
    ///
    /// # Errors
    ///
    /// Returns an error if query values cannot be converted to WASI data types.
    pub fn build(self) -> anyhow::Result<Query> {
        let mut statement = sea_query::Query::select();

        let column_specs = M::column_specs();

        for &field in M::projection() {
            if let Some(&(_, table, column)) = column_specs.iter().find(|&&(f, _, _)| f == field) {
                statement
                    .expr_as(SimpleExpr::Column(table_column(table, column)), Alias::new(field));
            } else {
                statement.column(table_column(M::TABLE, field));
            }
        }

        statement.from(Alias::new(M::TABLE));

        for JoinSpec {
            table,
            alias,
            on,
            kind,
        } in self.joins
        {
            let table_alias = Alias::new(table);
            if let Some(alias) = alias {
                statement.join_as(kind, table_alias, Alias::new(alias), on);
            } else {
                statement.join(kind, table_alias, on);
            }
        }

        for filter in self.filters {
            statement.and_where(filter);
        }

        if let Some(limit) = self.limit {
            statement.limit(limit);
        }

        if let Some(offset) = self.offset {
            statement.offset(offset);
        }

        for (column, order) in self.order {
            statement.order_by(column, order);
        }

        finish(&statement, M::TABLE, "select")
    }
}

pub fn table_column(table: &str, column: &str) -> ColumnRef {
    (Alias::new(table), Alias::new(column)).into()
}

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::Stop;
    use super::*;
    use crate::orm::DataType;

    #[test]
    fn where_order_limit_compose() {
        let query = SelectBuilder::<Stop>::new()
            .r#where(Filter::eq("stop_id", 7))
            .order_by_desc(None, "name")
            .limit(10)
            .build()
            .unwrap();

        assert_eq!(
            query.sql,
            r#"SELECT "stop"."stop_id", "stop"."name" FROM "stop" WHERE ("stop"."stop_id") = ($1) ORDER BY "stop"."name" DESC LIMIT $2"#
        );
        assert!(matches!(query.params[0], DataType::Int32(Some(7))));
        assert!(matches!(query.params[1], DataType::Uint64(Some(10))));
    }

    #[test]
    fn order_by_other_table() {
        let query =
            SelectBuilder::<Stop>::new().order_by_desc(Some("zone"), "name").build().unwrap();
        assert!(query.sql.ends_with(r#"ORDER BY "zone"."name" DESC"#), "sql: {}", query.sql);
    }
}
