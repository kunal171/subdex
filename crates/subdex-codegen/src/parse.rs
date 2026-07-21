//! Parse a `schema.graphql` (the Subsquid-compatible `@entity` dialect) into the
//! [`Schema`] entity model.
//!
//! Supported in v1 (see RFC 034):
//! - `type X @entity { … }` → one entity/table. An `id: ID!` field is required.
//! - Scalars: `ID String Int BigInt Float Boolean DateTime Bytes JSON`.
//! - **Lists of scalars** (`[String!]!`, `[Int]`) → native Postgres arrays.
//! - Field directives: `@index` (non-unique index), `@unique` (UNIQUE).
//! - `enum X { A B }` → stored as `TEXT`.
//! - A field whose type is another `@entity` is a **relation stored as that
//!   entity's id string** (no joins in v1).
//! - `@derivedFrom` fields are **skipped**: they're virtual reverse relations
//!   ("every X pointing at me"), so they generate no column. Serving them as
//!   GraphQL fields is a follow-up.
//!
//! Anything outside that surface is a clear [`ParseError`] rather than silently
//! ignored, so a builder never gets a table that doesn't match their schema.
//!
//! Validated against a real 103-entity / 35-enum production schema.

use crate::model::{table_name, to_snake_case, Entity, EnumDef, Field, FieldType, Scalar, Schema};
use async_graphql_parser::types::{BaseType, TypeKind, TypeSystemDefinition};
use thiserror::Error;

/// Why a schema couldn't be turned into an entity model. Every variant names the
/// offending type/field so the message is actionable.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("GraphQL syntax error: {0}")]
    Syntax(String),

    #[error(
        "entity `{entity}` has no `id: ID!` field (every @entity needs one as its primary key)"
    )]
    MissingId { entity: String },

    #[error("entity `{entity}` field `{field}`: `id` must be `ID!` (non-null)")]
    NullableId { entity: String, field: String },

    #[error(
        "entity `{entity}` field `{field}`: unknown type `{ty}` — expected a scalar \
         (ID, String, Int, BigInt, Float, Boolean, DateTime, Bytes, JSON), an enum, \
         or another @entity declared in this schema"
    )]
    UnknownType {
        entity: String,
        field: String,
        ty: String,
    },

    #[error(
        "entity `{entity}` field `{field}`: a list of `{inner}` isn't storable as a column — \
         lists of **scalars** are supported (stored as a Postgres array), but a list of \
         entities is a relation: put the foreign key on the other entity and mark this \
         field `@derivedFrom(field: \"…\")`"
    )]
    ListNotSupported {
        entity: String,
        field: String,
        inner: String,
    },

    #[error("duplicate entity `{0}` (declared more than once)")]
    DuplicateEntity(String),

    #[error("entity `{entity}` has duplicate field `{field}`")]
    DuplicateField { entity: String, field: String },

    #[error("no `@entity` types found — mark at least one type with `@entity`")]
    NoEntities,
}

/// Parse a schema document into the entity model.
pub fn parse_schema(source: &str) -> Result<Schema, ParseError> {
    let doc = async_graphql_parser::parse_schema(source)
        .map_err(|e| ParseError::Syntax(e.to_string()))?;

    // Pass 1: collect enum names and entity names so field types can be resolved
    // against them (a field may reference a type declared later in the file).
    let mut enums: Vec<EnumDef> = Vec::new();
    let mut entity_names: Vec<String> = Vec::new();

    for def in &doc.definitions {
        let TypeSystemDefinition::Type(t) = def else {
            continue;
        };
        let name = t.node.name.node.to_string();
        match &t.node.kind {
            TypeKind::Object(_) if has_directive(&t.node.directives, "entity") => {
                if entity_names.contains(&name) {
                    return Err(ParseError::DuplicateEntity(name));
                }
                entity_names.push(name);
            }
            TypeKind::Enum(e) => {
                enums.push(EnumDef {
                    name,
                    values: e
                        .values
                        .iter()
                        .map(|v| v.node.value.node.to_string())
                        .collect(),
                });
            }
            // Non-@entity object types, interfaces, unions, inputs, scalars are
            // ignored — they may exist for other tooling.
            _ => {}
        }
    }

    if entity_names.is_empty() {
        return Err(ParseError::NoEntities);
    }

    // Pass 2: build each entity's fields, resolving types.
    let mut entities = Vec::new();
    for def in &doc.definitions {
        let TypeSystemDefinition::Type(t) = def else {
            continue;
        };
        let TypeKind::Object(obj) = &t.node.kind else {
            continue;
        };
        if !has_directive(&t.node.directives, "entity") {
            continue;
        }
        let entity_name = t.node.name.node.to_string();

        let mut fields: Vec<Field> = Vec::new();
        for f in &obj.fields {
            let field_name = f.node.name.node.to_string();
            if fields.iter().any(|x| x.name == field_name) {
                return Err(ParseError::DuplicateField {
                    entity: entity_name.clone(),
                    field: field_name,
                });
            }

            // `@derivedFrom` marks a **virtual** reverse relation ("all the Xs
            // that point at me"). It is not a column — the data lives on the
            // other entity's foreign key — so we skip it entirely rather than
            // trying to store a list. (Serving these as GraphQL fields is a
            // follow-up; see RFC 034.)
            if has_directive(&f.node.directives, "derivedFrom") {
                continue;
            }

            let ty = &f.node.ty.node;
            let nullable = ty.nullable;
            let is_id = field_name == "id";

            let field_ty = match &ty.base {
                BaseType::Named(n) => {
                    let type_name = n.to_string();
                    if let Some(s) = Scalar::from_graphql(&type_name) {
                        FieldType::Scalar(s)
                    } else if enums.iter().any(|e| e.name == type_name) {
                        FieldType::Enum(type_name)
                    } else if entity_names.contains(&type_name) {
                        FieldType::Relation(type_name)
                    } else {
                        return Err(ParseError::UnknownType {
                            entity: entity_name.clone(),
                            field: field_name,
                            ty: type_name,
                        });
                    }
                }
                // A list of SCALARS is a native Postgres array (TEXT[], INTEGER[]).
                // A list of entities is a relation and belongs on the other side
                // (that's what `@derivedFrom`, skipped above, expresses).
                BaseType::List(inner) => match &inner.base {
                    BaseType::Named(n) => {
                        let inner_name = n.to_string();
                        match Scalar::from_graphql(&inner_name) {
                            Some(s) => FieldType::ScalarList(s),
                            None => {
                                return Err(ParseError::ListNotSupported {
                                    entity: entity_name.clone(),
                                    field: field_name,
                                    inner: inner_name,
                                })
                            }
                        }
                    }
                    BaseType::List(_) => {
                        return Err(ParseError::ListNotSupported {
                            entity: entity_name.clone(),
                            field: field_name,
                            inner: "nested list".to_string(),
                        })
                    }
                },
            };

            // `id` must be non-null (it's the primary key).
            if is_id && nullable {
                return Err(ParseError::NullableId {
                    entity: entity_name.clone(),
                    field: field_name,
                });
            }

            fields.push(Field {
                column: to_snake_case(&field_name),
                name: field_name,
                ty: field_ty,
                nullable,
                // `id` is the PK: implicitly unique and indexed, not via directives.
                indexed: has_directive(&f.node.directives, "index"),
                unique: has_directive(&f.node.directives, "unique"),
                is_id,
            });
        }

        if !fields.iter().any(|f| f.is_id) {
            return Err(ParseError::MissingId {
                entity: entity_name,
            });
        }

        entities.push(Entity {
            table: table_name(&entity_name),
            name: entity_name,
            fields,
        });
    }

    Ok(Schema { entities, enums })
}

/// Is `name` among these directives? (case-sensitive, as GraphQL is)
fn has_directive(
    directives: &[async_graphql_parser::Positioned<async_graphql_parser::types::ConstDirective>],
    name: &str,
) -> bool {
    directives.iter().any(|d| d.node.name.node == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        "A token movement."
        type Transfer @entity {
            id: ID!
            blockHeight: Int!
            from: String! @index
            to: String
            amount: BigInt!
            direction: Direction!
            success: Boolean
            payload: JSON
        }

        enum Direction { DEPOSIT WITHDRAW }

        type Account @entity {
            id: ID!
            handle: String @unique @index
            lastTransfer: Transfer
        }
    "#;

    #[test]
    fn parses_entities_fields_and_enums() {
        let s = parse_schema(VALID).expect("valid schema");
        assert_eq!(s.entities.len(), 2);
        assert_eq!(s.enums.len(), 1);
        assert_eq!(s.enums[0].values, vec!["DEPOSIT", "WITHDRAW"]);

        let t = s.entity("Transfer").unwrap();
        assert_eq!(t.table, "transfers");
        // id is the PK.
        assert!(t.id_field().is_some());
        // Field names → snake_case columns.
        let bh = t.fields.iter().find(|f| f.name == "blockHeight").unwrap();
        assert_eq!(bh.column, "block_height");
        assert!(!bh.nullable);
        assert_eq!(bh.rust_type(), "i32");
        // Nullable field wraps in Option.
        let to = t.fields.iter().find(|f| f.name == "to").unwrap();
        assert_eq!(to.rust_type(), "Option<String>");
        // BigInt is a decimal string / NUMERIC.
        let amt = t.fields.iter().find(|f| f.name == "amount").unwrap();
        assert_eq!(amt.rust_type(), "String");
        assert_eq!(amt.pg_type(), "NUMERIC");
        // Directives.
        let from = t.fields.iter().find(|f| f.name == "from").unwrap();
        assert!(from.indexed && !from.unique);
        // Enum field resolves to Enum, stored as TEXT.
        let dir = t.fields.iter().find(|f| f.name == "direction").unwrap();
        assert_eq!(dir.ty, FieldType::Enum("Direction".into()));
        assert_eq!(dir.pg_type(), "TEXT");
    }

    #[test]
    fn relation_field_resolves_to_the_referenced_entity() {
        let s = parse_schema(VALID).unwrap();
        let a = s.entity("Account").unwrap();
        let rel = a.fields.iter().find(|f| f.name == "lastTransfer").unwrap();
        // v1: a relation is the referenced entity's id string.
        assert_eq!(rel.ty, FieldType::Relation("Transfer".into()));
        assert_eq!(rel.rust_type(), "Option<String>");
        assert_eq!(rel.column, "last_transfer");
        // @unique + @index both read.
        let h = a.fields.iter().find(|f| f.name == "handle").unwrap();
        assert!(h.unique && h.indexed);
    }

    #[test]
    fn types_without_entity_directive_are_ignored() {
        let s = parse_schema(
            r#"
            type Transfer @entity { id: ID! }
            type NotAnEntity { id: ID! whatever: String }
        "#,
        )
        .unwrap();
        assert_eq!(s.entities.len(), 1, "only @entity types become tables");
        assert!(s.entity("NotAnEntity").is_none());
    }

    #[test]
    fn missing_id_is_an_error_naming_the_entity() {
        let err = parse_schema("type Transfer @entity { amount: BigInt! }").unwrap_err();
        assert_eq!(
            err,
            ParseError::MissingId {
                entity: "Transfer".into()
            }
        );
        assert!(err.to_string().contains("Transfer"));
    }

    #[test]
    fn nullable_id_is_rejected() {
        let err = parse_schema("type Transfer @entity { id: ID }").unwrap_err();
        assert!(matches!(err, ParseError::NullableId { .. }));
    }

    #[test]
    fn unknown_field_type_names_entity_field_and_type() {
        let err = parse_schema("type Transfer @entity { id: ID! x: Weird }").unwrap_err();
        match &err {
            ParseError::UnknownType { entity, field, ty } => {
                assert_eq!(entity, "Transfer");
                assert_eq!(field, "x");
                assert_eq!(ty, "Weird");
            }
            other => panic!("expected UnknownType, got {other:?}"),
        }
        // The message tells the builder what IS allowed.
        assert!(err.to_string().contains("BigInt"));
    }

    #[test]
    fn scalar_lists_become_postgres_arrays() {
        // Real schemas carry arrays of ids/addresses; Postgres stores these
        // natively, so they're columns — not an error.
        let s = parse_schema("type Swap @entity { id: ID! poolIds: [String!]! counts: [Int!] }")
            .expect("scalar lists are supported");
        let e = s.entity("Swap").unwrap();
        let pool_ids = e.fields.iter().find(|f| f.name == "poolIds").unwrap();
        assert_eq!(
            pool_ids.ty,
            FieldType::ScalarList(crate::model::Scalar::String)
        );
        assert_eq!(pool_ids.pg_type(), "TEXT[]");
        assert_eq!(pool_ids.rust_type(), "Vec<String>");
        assert_eq!(pool_ids.column, "pool_ids");
        // Nullable list wraps in Option.
        let counts = e.fields.iter().find(|f| f.name == "counts").unwrap();
        assert_eq!(counts.pg_type(), "INTEGER[]");
        assert_eq!(counts.rust_type(), "Option<Vec<i32>>");
    }

    #[test]
    fn a_list_of_entities_is_rejected_with_a_derivedfrom_hint() {
        // A list of *entities* is a relation — it belongs on the other side.
        let err = parse_schema(
            "type Account @entity { id: ID! transfers: [Transfer!]! }
             type Transfer @entity { id: ID! }",
        )
        .unwrap_err();
        assert!(matches!(err, ParseError::ListNotSupported { .. }));
        assert!(
            err.to_string().contains("@derivedFrom"),
            "the error should tell the builder what to do instead: {err}"
        );
    }

    #[test]
    fn derived_from_lists_are_skipped_not_errored() {
        // `@derivedFrom` is a VIRTUAL reverse relation — the data lives on the
        // other entity's FK, so it generates no column (and must not trip the
        // list check). This is the dominant list shape in real schemas.
        let s = parse_schema(
            r#"
            type Account @entity {
                id: ID!
                transfers: [Transfer!]! @derivedFrom(field: "owner")
            }
            type Transfer @entity {
                id: ID!
                owner: Account!
            }
        "#,
        )
        .expect("derivedFrom must not error");

        let a = s.entity("Account").unwrap();
        assert_eq!(a.fields.len(), 1, "only `id` becomes a column");
        assert!(
            a.fields.iter().all(|f| f.name != "transfers"),
            "derivedFrom field generates no column"
        );
        // The forward side is still a real relation column.
        let t = s.entity("Transfer").unwrap();
        assert!(t.fields.iter().any(|f| f.name == "owner"));
    }

    #[test]
    fn duplicate_field_is_rejected() {
        let err = parse_schema("type T @entity { id: ID! a: String a: Int }").unwrap_err();
        assert!(matches!(err, ParseError::DuplicateField { .. }));
    }

    #[test]
    fn schema_with_no_entities_is_an_error() {
        let err = parse_schema("type Plain { id: ID! }").unwrap_err();
        assert_eq!(err, ParseError::NoEntities);
    }

    #[test]
    fn syntax_error_is_surfaced() {
        let err = parse_schema("type Transfer @entity { id: ID!").unwrap_err();
        assert!(matches!(err, ParseError::Syntax(_)));
    }

    #[test]
    fn forward_references_resolve() {
        // `owner: Account` refers to a type declared *after* it.
        let s = parse_schema(
            r#"
            type Transfer @entity { id: ID! owner: Account! }
            type Account @entity { id: ID! }
        "#,
        )
        .unwrap();
        let t = s.entity("Transfer").unwrap();
        let o = t.fields.iter().find(|f| f.name == "owner").unwrap();
        assert_eq!(o.ty, FieldType::Relation("Account".into()));
    }
}
