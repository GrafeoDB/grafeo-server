import { useState, useCallback } from "react";
import { api, GrafeoApiError } from "../api/client";
import type { QueryRequest, QueryResponse } from "../types/api";

export function useQuery() {
  const [result, setResult] = useState<QueryResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  const execute = useCallback(async (query: string, language?: string, database?: string) => {
    setIsLoading(true);
    setError(null);
    try {
      const body: QueryRequest = {
        query,
        language: language as QueryRequest["language"],
      };
      if (database && database !== "default") {
        body.database = database;
      }
      const res = await api.query(body);
      setResult(res);
    } catch (err) {
      if (err instanceof GrafeoApiError) {
        setError(err.detail);
      } else {
        setError(String(err));
      }
      setResult(null);
    } finally {
      setIsLoading(false);
    }
  }, []);

  return { result, error, isLoading, execute };
}
