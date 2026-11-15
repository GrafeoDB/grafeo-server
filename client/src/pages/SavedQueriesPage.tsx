import { useNavigate } from "react-router-dom";
import { Button, DataTable, Page, type DataColumn } from "@grafeo-db/ui";
import { useWorkbench, type SavedQuery } from "../context/WorkbenchContext";
import { count, languageName } from "../shell/format";
import { Chip } from "./common";

type Row = SavedQuery & { index: number };

export default function SavedQueriesPage() {
  const navigate = useNavigate();
  const wb = useWorkbench();
  const rows: Row[] = wb.saved.map((q, index) => ({ ...q, index }));

  const open = (q: SavedQuery) => {
    wb.openQuery(q.query, q.language);
    navigate("/");
  };

  const columns: DataColumn<Row>[] = [
    {
      key: "name",
      header: "Name",
      render: (q) => (
        <span className="st-strong st-qname" title={q.name}>
          {q.name}
        </span>
      ),
    },
    { key: "language", header: "Language", render: (q) => <Chip>{languageName(q.language ?? "gql")}</Chip> },
    {
      key: "line",
      header: "First line",
      render: (q) => {
        const line = q.query.split("\n")[0];
        return (
          <span className="st-qline" title={q.query}>
            {line}
          </span>
        );
      },
    },
    {
      key: "actions",
      header: "",
      label: "Actions",
      render: (q) => (
        <span className="st-row-actions">
          <Button size="sm" onClick={() => open(q)}>
            Open
          </Button>
          <Button variant="ghost" size="sm" onClick={() => wb.removeSaved(q.index)}>
            Remove
          </Button>
        </span>
      ),
    },
  ];

  return (
    <Page
      title="Saved queries"
      meta={
        rows.length
          ? `${count(rows.length, "query", "queries")}, kept in this browser. Save in the query toolbar adds one.`
          : "Kept in this browser. Save in the query toolbar adds one."
      }
    >
      <div className="st-saved">
        <DataTable
          label="Saved queries"
          columns={columns}
          rows={rows}
          rowKey={(q) => `${q.index}-${q.name}`}
          empty="No saved queries yet. Save in the query toolbar adds the current query here."
        />
      </div>
    </Page>
  );
}
