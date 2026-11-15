import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { ThemeProvider } from "@grafeo-db/ui";
import { ServerProvider } from "./context/ServerContext";
import { WorkbenchProvider } from "./context/WorkbenchContext";
import { LabelStylesScope } from "./labelStyles";
import StudioShell from "./shell/StudioShell";
import DatabasesPage from "./pages/DatabasesPage";
import DatabaseDetailsPage from "./pages/DatabaseDetailsPage";
import TokensPage from "./pages/TokensPage";
import SavedQueriesPage from "./pages/SavedQueriesPage";

export default function App() {
  return (
    <ThemeProvider>
      <BrowserRouter basename="/studio">
        <ServerProvider>
          <WorkbenchProvider>
            <LabelStylesScope>
              <Routes>
                <Route element={<StudioShell />}>
                  <Route index element={null} />
                  <Route path="databases" element={<DatabasesPage />} />
                  <Route path="databases/:name" element={<DatabaseDetailsPage />} />
                  <Route path="tokens" element={<TokensPage />} />
                  <Route path="saved-queries" element={<SavedQueriesPage />} />
                  {/* Old addresses, for bookmarks */}
                  <Route path="admin" element={<Navigate to="/databases" replace />} />
                  <Route path="admin/dashboard" element={<Navigate to="/databases" replace />} />
                  <Route path="admin/databases" element={<Navigate to="/databases" replace />} />
                  <Route path="admin/backups" element={<Navigate to="/databases" replace />} />
                  <Route path="admin/tokens" element={<Navigate to="/tokens" replace />} />
                  <Route path="*" element={<Navigate to="/" replace />} />
                </Route>
              </Routes>
            </LabelStylesScope>
          </WorkbenchProvider>
        </ServerProvider>
      </BrowserRouter>
    </ThemeProvider>
  );
}
