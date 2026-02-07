import {
  StreamLanguage,
  type StreamParser,
} from "@codemirror/language";
import type { Extension } from "@codemirror/state";

interface SimpleState {
  inString: false | '"' | "'";
  inComment: boolean;
}

function makeStreamParser(keywords: Set<string>, extra?: {
  lineComment?: string[];
  variablePrefix?: string[];
}): StreamParser<SimpleState> {
  const lineComments = extra?.lineComment ?? ["//"];
  const varPrefixes = extra?.variablePrefix ?? [];

  return {
    startState: () => ({ inString: false, inComment: false }),
    token(stream, state) {
      // Block comment
      if (state.inComment) {
        if (stream.match("*/")) {
          state.inComment = false;
        } else {
          stream.next();
        }
        return "comment";
      }

      // String continuation
      if (state.inString) {
        const quote = state.inString;
        while (!stream.eol()) {
          const ch = stream.next();
          if (ch === "\\") {
            stream.next(); // skip escaped char
          } else if (ch === quote) {
            state.inString = false;
            break;
          }
        }
        return "string";
      }

      // Skip whitespace
      if (stream.eatSpace()) return null;

      // Line comments
      for (const lc of lineComments) {
        if (stream.match(lc)) {
          stream.skipToEnd();
          return "comment";
        }
      }

      // Block comment start
      if (stream.match("/*")) {
        state.inComment = true;
        return "comment";
      }

      // Strings
      const ch = stream.peek();
      if (ch === '"' || ch === "'") {
        state.inString = ch as '"' | "'";
        stream.next();
        return "string";
      }

      // Numbers
      if (stream.match(/^-?\d+(\.\d+)?/)) return "number";

      // Variable prefixes ($param, ?var)
      for (const prefix of varPrefixes) {
        if (stream.match(new RegExp(`^\\${prefix}[A-Za-z_][A-Za-z0-9_]*`))) {
          return "variableName.special";
        }
      }

      // Words (keywords or identifiers)
      if (stream.match(/^[A-Za-z_][A-Za-z0-9_]*/)) {
        const word = stream.current();
        if (keywords.has(word.toUpperCase()) || keywords.has(word)) {
          return "keyword";
        }
        return "variableName";
      }

      // Operators and punctuation
      stream.next();
      return null;
    },
  };
}

const GQL_KEYWORDS = new Set([
  "MATCH", "RETURN", "WHERE", "INSERT", "DELETE", "SET", "REMOVE",
  "WITH", "OPTIONAL", "UNION", "ORDER", "BY", "LIMIT", "SKIP",
  "DISTINCT", "AS", "AND", "OR", "NOT", "IN", "IS", "NULL",
  "TRUE", "FALSE", "COUNT", "SUM", "AVG", "MIN", "MAX",
  "EXISTS", "CASE", "WHEN", "THEN", "ELSE", "END",
  "ASC", "DESC", "FILTER", "LET", "FOR",
]);

const CYPHER_KEYWORDS = new Set([
  ...GQL_KEYWORDS,
  "CREATE", "MERGE", "DETACH", "CALL", "YIELD", "UNWIND",
  "FOREACH", "ON", "STARTS", "ENDS", "CONTAINS",
]);

const GRAPHQL_KEYWORDS = new Set([
  "query", "mutation", "subscription", "type", "input",
  "enum", "fragment", "on", "implements", "extend",
  "schema", "directive", "interface", "union", "scalar",
  "true", "false", "null",
]);

const GREMLIN_KEYWORDS = new Set([
  "g", "V", "E", "has", "hasLabel", "hasId", "hasKey", "hasValue",
  "values", "valueMap", "out", "in", "both", "outE", "inE", "bothE",
  "outV", "inV", "bothV", "addV", "addE", "property", "to", "from",
  "count", "limit", "select", "by", "where", "fold", "unfold",
  "dedup", "path", "group", "groupCount", "order", "range",
  "as", "repeat", "until", "emit", "times", "is", "not",
  "and", "or", "coalesce", "choose", "constant", "identity",
]);

const SPARQL_KEYWORDS = new Set([
  "SELECT", "CONSTRUCT", "ASK", "DESCRIBE", "WHERE", "FILTER",
  "OPTIONAL", "UNION", "PREFIX", "BASE", "INSERT", "DELETE",
  "DATA", "GRAPH", "FROM", "NAMED", "ORDER", "BY", "LIMIT",
  "OFFSET", "BIND", "VALUES", "GROUP", "HAVING", "AS",
  "DISTINCT", "REDUCED", "SERVICE", "MINUS", "NOT", "EXISTS",
  "IN", "STR", "LANG", "DATATYPE", "BOUND", "SAMETERM",
  "ISURI", "ISBLANK", "ISLITERAL", "REGEX", "TRUE", "FALSE",
  "A",
]);

const gqlParser = makeStreamParser(GQL_KEYWORDS, {
  lineComment: ["//", "#"],
});

const cypherParser = makeStreamParser(CYPHER_KEYWORDS, {
  lineComment: ["//"],
  variablePrefix: ["$"],
});

const graphqlParser = makeStreamParser(GRAPHQL_KEYWORDS, {
  lineComment: ["#"],
  variablePrefix: ["$"],
});

const gremlinParser = makeStreamParser(GREMLIN_KEYWORDS, {
  lineComment: ["//"],
});

const sparqlParser = makeStreamParser(SPARQL_KEYWORDS, {
  lineComment: ["#"],
  variablePrefix: ["?", "$"],
});

const languageExtensions: Record<string, Extension> = {
  gql: StreamLanguage.define(gqlParser),
  cypher: StreamLanguage.define(cypherParser),
  graphql: StreamLanguage.define(graphqlParser),
  gremlin: StreamLanguage.define(gremlinParser),
  sparql: StreamLanguage.define(sparqlParser),
};

export function getLanguageExtension(lang: string): Extension {
  return languageExtensions[lang] ?? languageExtensions.gql;
}
