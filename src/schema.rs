//! Schema file parsing and loading for database creation.
//!
//! Each schema type is behind a feature flag. The engine database is already
//! created before these functions are called; on failure the caller deletes it.

use grafeo_engine::GrafeoDB;

use crate::error::ApiError;
use crate::routes::DatabaseType;

/// Dispatches schema loading based on database type. No-op for types that
/// don't use schemas (Lpg, Rdf).
pub fn load_schema(db_type: DatabaseType, content: &[u8], db: &GrafeoDB) -> Result<(), ApiError> {
    match db_type {
        DatabaseType::Lpg | DatabaseType::Rdf => Ok(()),
        DatabaseType::OwlSchema => load_owl(content, db),
        DatabaseType::RdfsSchema => load_rdfs(content, db),
        DatabaseType::JsonSchema => load_json_schema(content, db),
    }
}

/// Load an OWL ontology into an RDF database.
#[cfg(feature = "owl-schema")]
fn load_owl(content: &[u8], db: &GrafeoDB) -> Result<(), ApiError> {
    use sophia_api::prelude::*;
    use sophia_turtle::parser::turtle;

    // Try parsing as Turtle first (most common for OWL files)
    let reader = std::io::Cursor::new(content);
    let triples: Result<Vec<[sophia_api::term::SimpleTerm<'static>; 3]>, _> =
        turtle::parse_bufread(reader).collect_triples();

    let triples =
        triples.map_err(|e| ApiError::BadRequest(format!("failed to parse OWL schema: {e}")))?;

    // Insert each triple as a SPARQL INSERT
    let session = db.session();
    for triple in &triples {
        let s = format_term(&triple[0]);
        let p = format_term(&triple[1]);
        let o = format_term(&triple[2]);
        let sparql = format!("INSERT DATA {{ {s} {p} {o} . }}");
        session
            .execute_sparql(&sparql)
            .map_err(|e| ApiError::Internal(format!("failed to insert OWL triple: {e}")))?;
    }

    tracing::info!(triple_count = triples.len(), "Loaded OWL schema");
    Ok(())
}

#[cfg(not(feature = "owl-schema"))]
fn load_owl(_content: &[u8], _db: &GrafeoDB) -> Result<(), ApiError> {
    Err(ApiError::BadRequest(
        "OWL Schema support requires the 'owl-schema' feature".to_string(),
    ))
}

/// Load an RDFS schema into an RDF database.
#[cfg(feature = "rdfs-schema")]
fn load_rdfs(content: &[u8], db: &GrafeoDB) -> Result<(), ApiError> {
    // RDFS files are also Turtle/RDF, reuse the same parsing logic as OWL
    load_owl(content, db)
}

#[cfg(not(feature = "rdfs-schema"))]
fn load_rdfs(_content: &[u8], _db: &GrafeoDB) -> Result<(), ApiError> {
    Err(ApiError::BadRequest(
        "RDFS Schema support requires the 'rdfs-schema' feature".to_string(),
    ))
}

/// Load a JSON Schema and create LPG catalog constraints.
#[cfg(feature = "json-schema")]
fn load_json_schema(content: &[u8], db: &GrafeoDB) -> Result<(), ApiError> {
    let schema_value: serde_json::Value = serde_json::from_slice(content)
        .map_err(|e| ApiError::BadRequest(format!("invalid JSON in schema file: {e}")))?;

    // Validate that it looks like a JSON Schema
    if !schema_value.is_object() {
        return Err(ApiError::BadRequest(
            "schema file must be a JSON object".to_string(),
        ));
    }

    // Extract definitions/properties to create node labels and constraints
    let session = db.session();
    let mut label_count = 0;

    // Process top-level "definitions" or "$defs" (JSON Schema draft-07 / 2020-12)
    let defs = schema_value
        .get("definitions")
        .or_else(|| schema_value.get("$defs"));

    if let Some(defs) = defs
        && let Some(obj) = defs.as_object()
    {
        for (type_name, _type_def) in obj {
            // Create a node with the label matching the type name
            // This establishes the label in the catalog
            let cypher = format!(
                "CREATE (n:{} {{_schema: true}}) RETURN n",
                type_name.replace(' ', "_")
            );
            if let Err(e) = session.execute_cypher(&cypher) {
                tracing::warn!(
                    type_name = %type_name,
                    error = %e,
                    "Failed to create schema label"
                );
            } else {
                label_count += 1;
            }
        }
    }

    // If the schema itself has a "title", create that as a label too
    if let Some(title) = schema_value.get("title").and_then(|t| t.as_str()) {
        let cypher = format!(
            "CREATE (n:{} {{_schema: true}}) RETURN n",
            title.replace(' ', "_")
        );
        let _ = session.execute_cypher(&cypher);
        label_count += 1;
    }

    tracing::info!(label_count, "Loaded JSON Schema constraints");
    Ok(())
}

#[cfg(not(feature = "json-schema"))]
fn load_json_schema(_content: &[u8], _db: &GrafeoDB) -> Result<(), ApiError> {
    Err(ApiError::BadRequest(
        "JSON Schema support requires the 'json-schema' feature".to_string(),
    ))
}

/// Formats an RDF term for use in a SPARQL query string.
#[cfg(feature = "owl-schema")]
fn format_term<T: sophia_api::term::Term>(term: &T) -> String {
    use sophia_api::term::TermKind;
    match term.kind() {
        TermKind::Iri => format!("<{}>", term.iri().unwrap()),
        TermKind::Literal => {
            let lex = term.lexical_form().unwrap();
            if let Some(lang) = term.language_tag() {
                format!("\"{}\"@{}", lex, lang.as_str())
            } else if let Some(dt) = term.datatype() {
                format!("\"{}\"^^<{}>", lex, dt)
            } else {
                format!("\"{}\"", lex)
            }
        }
        TermKind::BlankNode => {
            let id = term.bnode_id().unwrap();
            format!("_:{}", id.as_str())
        }
        _ => {
            if let Some(lex) = term.lexical_form() {
                format!("\"{}\"", lex)
            } else {
                "\"\"".to_string()
            }
        }
    }
}
