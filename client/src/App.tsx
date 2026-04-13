import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { AppProvider } from "./context/AppContext";
import { WorkbenchProvider } from "./context/WorkbenchContext";
import Layout from "./components/Layout/Layout";
import WorkbenchView from "./views/WorkbenchView";
import DatabasesList from "./views/databases/DatabasesList";
import DatabaseDetails from "./views/databases/DatabaseDetails";
import TokensView from "./views/TokensView";

export default function App() {
  return (
    <AppProvider>
      <WorkbenchProvider>
        <BrowserRouter basename="/studio">
          <Routes>
            <Route element={<Layout />}>
              <Route path="/" element={<WorkbenchView />} />
              <Route path="/databases" element={<DatabasesList />} />
              <Route path="/databases/:name" element={<DatabaseDetails />} />
              <Route path="/tokens" element={<TokensView />} />
              {/* Legacy /admin/* redirects for anyone with bookmarked URLs */}
              <Route path="/admin" element={<Navigate to="/databases" replace />} />
              <Route
                path="/admin/dashboard"
                element={<Navigate to="/databases" replace />}
              />
              <Route
                path="/admin/databases"
                element={<Navigate to="/databases" replace />}
              />
              <Route
                path="/admin/backups"
                element={<Navigate to="/databases" replace />}
              />
              <Route
                path="/admin/tokens"
                element={<Navigate to="/tokens" replace />}
              />
            </Route>
          </Routes>
        </BrowserRouter>
      </WorkbenchProvider>
    </AppProvider>
  );
}
