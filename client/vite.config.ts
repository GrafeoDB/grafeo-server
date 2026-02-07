import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "/studio/",
  server: {
    port: 5173,
    proxy: {
      "/query": "http://localhost:7474",
      "/cypher": "http://localhost:7474",
      "/graphql": "http://localhost:7474",
      "/gremlin": "http://localhost:7474",
      "/sparql": "http://localhost:7474",
      "/tx": "http://localhost:7474",
      "/db": "http://localhost:7474",
      "/health": "http://localhost:7474",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
