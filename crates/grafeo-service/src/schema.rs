//! Schema file parsing and loading for database creation.
//!
//! Each schema type is behind a feature flag. The engine database is already
//! created before these functions are called; on failure the caller deletes it.

use grafeo_engine::GrafeoDB;

use crate::error::ServiceError;
use crate::types::DatabaseType;

/// Validates that a string is a safe identifier for use in query labels.
///
/// Accepts only ASCII letters, digits, and underscores, starting with a letter
/// or underscore. Rejects anything that could be used for injection.
#[cfg(feature = "json-schema")]
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Dispatches schema loading based on database type. No-op for types that
/// don't use schemas (Lpg, Rdf).
pub fn load_schema(
    db_type: DatabaseType,
    content: &[u8],
    db: &GrafeoDB,
) -> Result<(), ServiceError> {
    match db_type {
        DatabaseType::Lpg | DatabaseType::Rdf => Ok(()),
        DatabaseType::OwlSchema => load_owl(content, db),
        DatabaseType::RdfsSchema => load_rdfs(content, db),
        DatabaseType::JsonSchema => load_json_schema(content, db),
    }
}

/// Load an OWL ontology into an RDF database.
#[cfg(feature = "owl-schema")]
fn load_owl(content: &[u8], db: &GrafeoDB) -> Result<(), ServiceError> {
    use sophia_api::prelude::*;
    use sophia_turtle::parser::turtle;

    // Try parsing as Turtle first (most common for OWL files)
    let reader = std::io::Cursor::new(content);
    let triples: Result<Vec<[sophia_api::term::SimpleTerm<'static>; 3]>, _> =
        turtle::parse_bufread(reader).collect_triples();

    let triples = triples
        .map_err(|e| ServiceError::BadRequest(format!("failed to parse OWL schema: {e}")))?;

    // Insert each triple as a SPARQL INSERT
    let session = db.session();
    for triple in &triples {
        let s = format_term(&triple[0])?;
        let p = format_term(&triple[1])?;
        let o = format_term(&triple[2])?;
        let sparql = format!("INSERT DATA {{ {s} {p} {o} . }}");
        session
            .execute_sparql(&sparql)
            .map_err(|e| ServiceError::Internal(format!("failed to insert OWL triple: {e}")))?;
    }

    tracing::info!(triple_count = triples.len(), "Loaded OWL schema");
    Ok(())
}

#[cfg(not(feature = "owl-schema"))]
fn load_owl(_content: &[u8], _db: &GrafeoDB) -> Result<(), ServiceError> {
    Err(ServiceError::BadRequest(
        "OWL Schema support requires the 'owl-schema' feature".to_string(),
    ))
}

/// Load an RDFS schema into an RDF database.
#[cfg(feature = "rdfs-schema")]
fn load_rdfs(content: &[u8], db: &GrafeoDB) -> Result<(), ServiceError> {
    // RDFS files are also Turtle/RDF, reuse the same parsing logic as OWL
    load_owl(content, db)
}

#[cfg(not(feature = "rdfs-schema"))]
fn load_rdfs(_content: &[u8], _db: &GrafeoDB) -> Result<(), ServiceError> {
    Err(ServiceError::BadRequest(
        "RDFS Schema support requires the 'rdfs-schema' feature".to_string(),
    ))
}

/// Load a JSON Schema and create LPG catalog constraints.
#[cfg(feature = "json-schema")]
fn load_json_schema(content: &[u8], db: &GrafeoDB) -> Result<(), ServiceError> {
    let schema_value: serde_json::Value = serde_json::from_slice(content)
        .map_err(|e| ServiceError::BadRequest(format!("invalid JSON in schema file: {e}")))?;

    // Validate that it looks like a JSON Schema
    if !schema_value.is_object() {
        return Err(ServiceError::BadRequest(
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
            let safe_name = type_name.replace(' ', "_");
            if !is_safe_identifier(&safe_name) {
                return Err(ServiceError::BadRequest(format!(
                    "invalid schema type name '{type_name}': must contain only \
                     ASCII letters, digits, and underscores"
                )));
            }
            let cypher = format!("CREATE (n:{safe_name} {{_schema: true}}) RETURN n");
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
        let safe_title = title.replace(' ', "_");
        if !is_safe_identifier(&safe_title) {
            return Err(ServiceError::BadRequest(format!(
                "invalid schema title '{title}': must contain only \
                 ASCII letters, digits, and underscores"
            )));
        }
        let cypher = format!("CREATE (n:{safe_title} {{_schema: true}}) RETURN n");
        let _ = session.execute_cypher(&cypher);
        label_count += 1;
    }

    tracing::info!(label_count, "Loaded JSON Schema constraints");
    Ok(())
}

#[cfg(not(feature = "json-schema"))]
fn load_json_schema(_content: &[u8], _db: &GrafeoDB) -> Result<(), ServiceError> {
    Err(ServiceError::BadRequest(
        "JSON Schema support requires the 'json-schema' feature".to_string(),
    ))
}

/// Formats an RDF term for a SPARQL `INSERT DATA` statement, or refuses one
/// that can't be written safely. Literals are escaped as SPARQL strings, so
/// no lexical form can end its literal; an IRI, language tag or blank node
/// label with a character SPARQL doesn't allow there is refused, since no
/// escape makes it valid.
#[cfg(feature = "owl-schema")]
fn format_term<T: sophia_api::term::Term>(term: &T) -> Result<String, ServiceError> {
    use sophia_api::term::TermKind;
    match term.kind() {
        TermKind::Iri => sparql_iri(term.iri().unwrap().as_str()),
        TermKind::Literal => {
            let lex = sparql_string(term.lexical_form().unwrap().as_ref());
            if let Some(lang) = term.language_tag() {
                let tag = lang.as_str();
                if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(invalid_term("language tag", tag));
                }
                Ok(format!("{lex}@{tag}"))
            } else if let Some(dt) = term.datatype() {
                Ok(format!("{lex}^^{}", sparql_iri(dt.as_str())?))
            } else {
                Ok(lex)
            }
        }
        TermKind::BlankNode => {
            let id = term.bnode_id().unwrap();
            let id = id.as_str();
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(invalid_term("blank node label", id));
            }
            Ok(format!("_:{id}"))
        }
        _ => Ok(term
            .lexical_form()
            .map_or_else(|| sparql_string(""), |lex| sparql_string(lex.as_ref()))),
    }
}

/// An IRI as a SPARQL `IRIREF`. One holding a character `IRIREF` excludes
/// (`<`, `>`, `"`, `{`, `}`, `|`, `^`, backquote, backslash, a control
/// character or a space) is refused.
#[cfg(feature = "owl-schema")]
fn sparql_iri(iri: &str) -> Result<String, ServiceError> {
    let forbidden =
        |c: char| matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\') || c <= ' ';
    if iri.chars().any(forbidden) {
        return Err(invalid_term("IRI", iri));
    }
    Ok(format!("<{iri}>"))
}

/// A string as a SPARQL `STRING_LITERAL_QUOTE`, with every character that
/// could end it, or the line, escaped.
#[cfg(feature = "owl-schema")]
fn sparql_string(lex: &str) -> String {
    let mut out = String::with_capacity(lex.len() + 2);
    out.push('"');
    for c in lex.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\u{8}' => out.push_str(r"\b"),
            '\u{c}' => out.push_str(r"\f"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(feature = "owl-schema")]
fn invalid_term(kind: &str, value: &str) -> ServiceError {
    ServiceError::BadRequest(format!("schema file holds an invalid {kind}: {value:?}"))
}

#[cfg(all(test, feature = "owl-schema"))]
mod tests {
    use super::*;

    fn objects(db: &GrafeoDB) -> Vec<String> {
        db.session()
            .execute_sparql("SELECT ?o WHERE { ?s ?p ?o }")
            .unwrap()
            .rows()
            .iter()
            .map(|row| format!("{row:?}"))
            .collect()
    }

    #[test]
    fn a_literal_cannot_end_its_statement() {
        let db = GrafeoDB::new_in_memory();
        // The literal's text closes a SPARQL literal and the INSERT DATA,
        // then opens a second one, with a backslash and a line break too.
        let turtle = r#"<http://ex/a> <http://ex/p> "x\" . } ; INSERT DATA { <http://ex/evil> <http://ex/p> \"pwn\\ \n" ."#;
        load_owl(turtle.as_bytes(), &db).unwrap();
        let objects = objects(&db);
        assert_eq!(
            objects.len(),
            1,
            "one triple, the literal intact: {objects:?}"
        );
        assert!(objects[0].contains("INSERT DATA"), "{objects:?}");
    }

    #[test]
    fn typed_and_tagged_literals_keep_their_type_and_tag() {
        let db = GrafeoDB::new_in_memory();
        let turtle = r#"<http://ex/a> <http://ex/p> "1"^^<http://www.w3.org/2001/XMLSchema#integer> , "hallo"@nl ."#;
        load_owl(turtle.as_bytes(), &db).unwrap();
        assert_eq!(objects(&db).len(), 2);
    }

    #[test]
    fn terms_sparql_cannot_hold_are_refused() {
        assert!(sparql_iri("http://ex/a>b").is_err());
        assert!(sparql_iri("http://ex/a b").is_err());
        assert!(sparql_iri("http://ex/a").is_ok());
        assert_eq!(sparql_string("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
    }
}
