//! The in-memory **entity model**: what a parsed `schema.graphql` becomes.
//!
//! This is the single intermediate representation every generator (Rust structs,
//! SQL migrations, upsert helpers, GraphQL types) reads. Keeping it separate from
//! the GraphQL AST means the generators never touch parser types, and the model
//! can be built by hand in tests.

/// A scalar field type supported by the v1 schema dialect.
///
/// The mapping to Rust and Postgres is fixed here so every generator agrees.
///
/// | GraphQL    | Rust           | Postgres  |
/// |------------|----------------|-----------|
/// | `ID`       | `String`       | `TEXT`    |
/// | `String`   | `String`       | `TEXT`    |
/// | `Int`      | `i32`          | `INTEGER` |
/// | `BigInt`   | `String`\*     | `NUMERIC` |
/// | `Float`    | `f64`          | `DOUBLE PRECISION` |
/// | `Boolean`  | `bool`         | `BOOLEAN` |
/// | `DateTime` | `i64` (ms)     | `BIGINT`  |
/// | `Bytes`    | `String` (hex) | `TEXT`    |
/// | `JSON`     | `serde_json::Value` | `JSONB` |
///
/// \* `BigInt` is carried as a **decimal string** rather than `i64`: chain
/// balances routinely exceed `i64::MAX`, and this is what the existing examples
/// already do (`amount` bound as `$n::text::numeric`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    Id,
    String,
    Int,
    BigInt,
    Float,
    Boolean,
    DateTime,
    Bytes,
    Json,
}

impl Scalar {
    /// Parse a GraphQL scalar name. Returns `None` for anything unknown (which
    /// the validator turns into a clear error, or treats as an enum/relation).
    pub fn from_graphql(name: &str) -> Option<Self> {
        Some(match name {
            "ID" => Self::Id,
            "String" => Self::String,
            "Int" => Self::Int,
            "BigInt" => Self::BigInt,
            "Float" => Self::Float,
            "Boolean" => Self::Boolean,
            "DateTime" => Self::DateTime,
            "Bytes" => Self::Bytes,
            "JSON" | "JSONObject" => Self::Json,
            _ => return None,
        })
    }

    /// The GraphQL spelling (for diagnostics and regenerating docs).
    pub fn graphql_name(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::String => "String",
            Self::Int => "Int",
            Self::BigInt => "BigInt",
            Self::Float => "Float",
            Self::Boolean => "Boolean",
            Self::DateTime => "DateTime",
            Self::Bytes => "Bytes",
            Self::Json => "JSON",
        }
    }

    /// The Rust type for a **non-null** field of this scalar. A nullable field
    /// wraps this in `Option<…>` (see [`Field::rust_type`]).
    pub fn rust_type(self) -> &'static str {
        match self {
            Self::Id | Self::String | Self::BigInt | Self::Bytes => "String",
            Self::Int => "i32",
            Self::Float => "f64",
            Self::Boolean => "bool",
            Self::DateTime => "i64",
            Self::Json => "serde_json::Value",
        }
    }

    /// The Postgres column type.
    pub fn pg_type(self) -> &'static str {
        match self {
            Self::Id | Self::String | Self::Bytes => "TEXT",
            Self::Int => "INTEGER",
            Self::BigInt => "NUMERIC",
            Self::Float => "DOUBLE PRECISION",
            Self::Boolean => "BOOLEAN",
            Self::DateTime => "BIGINT",
            Self::Json => "JSONB",
        }
    }
}

/// What a field holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    /// A built-in scalar.
    Scalar(Scalar),
    /// A **list of scalars**, e.g. `[String!]!` or `[Int]!` — stored natively as
    /// a Postgres array (`TEXT[]`, `INTEGER[]`) and a `Vec<T>` in Rust.
    ///
    /// Only scalar element types are supported: a list of *entities* is a
    /// relation and belongs on the other side (see `@derivedFrom`).
    ScalarList(Scalar),
    /// A user-defined `enum` from the same schema. Stored as `TEXT`.
    Enum(String),
    /// A reference to another `@entity`.
    ///
    /// **v1 stores the referenced entity's `id` as a string** — no joins, no
    /// `@derivedFrom` magic (see RFC 034). The generated column is `TEXT` and the
    /// generated Rust field is a `String` id, named as written.
    Relation(String),
}

/// One field of an entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field name as written in the schema (camelCase by GraphQL convention).
    pub name: String,
    /// The column name to use in Postgres (snake_case of `name`).
    pub column: String,
    pub ty: FieldType,
    /// `false` when the schema wrote `Type!`.
    pub nullable: bool,
    /// `@index` — generate a non-unique index on this column.
    pub indexed: bool,
    /// `@unique` — generate a UNIQUE constraint.
    pub unique: bool,
    /// `true` for the entity's `id: ID!` primary key.
    pub is_id: bool,
}

impl Field {
    /// The Rust type, wrapped in `Option<…>` when nullable.
    pub fn rust_type(&self) -> String {
        let base = match &self.ty {
            FieldType::Scalar(s) => s.rust_type().to_string(),
            FieldType::ScalarList(s) => format!("Vec<{}>", s.rust_type()),
            // Enums and relations are both carried as strings in v1.
            FieldType::Enum(_) | FieldType::Relation(_) => "String".to_string(),
        };
        if self.nullable {
            format!("Option<{base}>")
        } else {
            base
        }
    }

    /// The Postgres column type (nullability is expressed by `NOT NULL`, not the
    /// type, so this is the same either way).
    pub fn pg_type(&self) -> String {
        match &self.ty {
            FieldType::Scalar(s) => s.pg_type().to_string(),
            // Postgres arrays: TEXT[], INTEGER[], NUMERIC[], …
            FieldType::ScalarList(s) => format!("{}[]", s.pg_type()),
            FieldType::Enum(_) | FieldType::Relation(_) => "TEXT".to_string(),
        }
    }
}

/// A user-defined enum in the schema. Values are stored as `TEXT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDef {
    pub name: String,
    pub values: Vec<String>,
}

/// One `@entity` type → one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    /// Type name as written (PascalCase by convention), e.g. `UserProfile`.
    pub name: String,
    /// The table name (snake_case + pluralised), e.g. `user_profiles`.
    pub table: String,
    pub fields: Vec<Field>,
}

impl Entity {
    /// The primary-key field (`id: ID!`). Guaranteed present by validation.
    pub fn id_field(&self) -> Option<&Field> {
        self.fields.iter().find(|f| f.is_id)
    }
}

/// The whole parsed schema: every entity and enum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    pub entities: Vec<Entity>,
    pub enums: Vec<EnumDef>,
}

impl Schema {
    pub fn entity(&self, name: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.name == name)
    }
    pub fn has_enum(&self, name: &str) -> bool {
        self.enums.iter().any(|e| e.name == name)
    }
}

/// Convert a camelCase / PascalCase identifier to snake_case.
///
/// `userAddress` → `user_address`, `assetId` → `asset_id`, `ID` → `id`.
/// Consecutive capitals are treated as a unit (`HTTPServer` → `http_server`).
pub fn to_snake_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_ascii_lowercase();
            let prev_upper = i > 0 && chars[i - 1].is_ascii_uppercase();
            if i > 0 && (prev_lower || (prev_upper && next_lower)) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Derive a table name from an entity name: snake_case, naively pluralised.
///
/// `Transfer` → `transfers`, `UserProfile` → `user_profiles`,
/// `AssetClass` → `asset_classes`, `Identity` → `identities`.
pub fn table_name(entity: &str) -> String {
    let base = to_snake_case(entity);
    if base.ends_with('s') || base.ends_with("ch") || base.ends_with("sh") || base.ends_with('x') {
        format!("{base}es")
    } else if base.ends_with('y')
        && !base.ends_with("ay")
        && !base.ends_with("ey")
        && !base.ends_with("oy")
        && !base.ends_with("uy")
    {
        format!("{}ies", &base[..base.len() - 1])
    } else {
        format!("{base}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_conversions() {
        assert_eq!(to_snake_case("userAddress"), "user_address");
        assert_eq!(to_snake_case("assetId"), "asset_id");
        assert_eq!(to_snake_case("id"), "id");
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("blockHeight"), "block_height");
        // Consecutive capitals stay together until a lowercase starts a new word.
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
    }

    #[test]
    fn table_names_are_pluralised() {
        assert_eq!(table_name("Transfer"), "transfers");
        assert_eq!(table_name("UserProfile"), "user_profiles");
        assert_eq!(table_name("AssetClass"), "asset_classes");
        assert_eq!(table_name("Identity"), "identities");
        // …but not for a vowel+y (day -> days, not daies).
        assert_eq!(table_name("Day"), "days");
    }

    #[test]
    fn scalar_mapping_is_stable() {
        assert_eq!(Scalar::from_graphql("BigInt"), Some(Scalar::BigInt));
        assert_eq!(Scalar::from_graphql("Nope"), None);
        // BigInt is a decimal string in Rust, NUMERIC in PG (balances exceed i64).
        assert_eq!(Scalar::BigInt.rust_type(), "String");
        assert_eq!(Scalar::BigInt.pg_type(), "NUMERIC");
        assert_eq!(Scalar::DateTime.rust_type(), "i64");
        assert_eq!(Scalar::Json.pg_type(), "JSONB");
    }

    #[test]
    fn nullable_fields_wrap_in_option() {
        let mut f = Field {
            name: "bio".into(),
            column: "bio".into(),
            ty: FieldType::Scalar(Scalar::String),
            nullable: true,
            indexed: false,
            unique: false,
            is_id: false,
        };
        assert_eq!(f.rust_type(), "Option<String>");
        f.nullable = false;
        assert_eq!(f.rust_type(), "String");
    }

    #[test]
    fn enums_and_relations_are_text_strings() {
        let f = Field {
            name: "owner".into(),
            column: "owner".into(),
            ty: FieldType::Relation("UserProfile".into()),
            nullable: false,
            indexed: false,
            unique: false,
            is_id: false,
        };
        // v1: a relation is the referenced entity's id, stored as TEXT.
        assert_eq!(f.rust_type(), "String");
        assert_eq!(f.pg_type(), "TEXT");
    }
}
