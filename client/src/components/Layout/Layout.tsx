import { Outlet, useLocation } from "react-router-dom";
import Sidebar from "../Sidebar/Sidebar";
import TopNav from "../TopNav/TopNav";
import { useApp } from "../../context/AppContext";
import styles from "./Layout.module.css";

export default function Layout() {
  const { sidebarOpen, toggleSidebar, currentDatabase, selectDatabase } = useApp();
  const location = useLocation();
  // Sidebar is only meaningful on the workbench route (/).
  const showSidebar = location.pathname === "/";

  return (
    <div className={styles.layout}>
      <TopNav />
      <div className={styles.body}>
        {showSidebar && (
          <Sidebar
            collapsed={!sidebarOpen}
            onToggle={toggleSidebar}
            currentDatabase={currentDatabase}
            onSelectDatabase={selectDatabase}
          />
        )}
        <div className={styles.main}>
          <Outlet />
        </div>
      </div>
    </div>
  );
}
