import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";
import { AssetsPage, ObjectPage } from "./pages/assets/AssetPages";
import { KbArticlePage, KbEditPage, KbNewPage, KbPage } from "./pages/kb/KbPages";
import { ReportsPage } from "./pages/reports/ReportsPage";
import { AssetTypesAdminPage, AutomationAdminPage, IntegrationsPage, SlaAdminPage, TemplatesAdminPage } from "./pages/admin/ServicePages";
import { Layout, ModuleGate, NotFound, PermGate } from "./layout";
import { adminGroups } from "./pages/admin/adminMenu";
import type { ModuleKey } from "./session";
import { AdPage } from "./pages/admin/AdPage";
import { AdminHomePage } from "./pages/admin/AdminHomePage";
import { HelpdeskAdminPage } from "./pages/admin/HelpdeskAdminPage";
import { NewTicketPage } from "./pages/helpdesk/NewTicketPage";
import { TicketResponsePage } from "./pages/helpdesk/PublicPages";
import { TicketPage } from "./pages/helpdesk/TicketPage";
import { DeskPage, MyTicketsPage } from "./pages/helpdesk/TicketsPage";
import { MailPage } from "./pages/admin/MailPage";
import { LinkingPage, PhonebookAdminPage } from "./pages/admin/PhonebookAdminPage";
import { AuditPage, SecurityPage, SystemPage, TelephonyPage } from "./pages/admin/SettingsPages";
import { SitesPage } from "./pages/admin/SitesPage";
import { RolesPage, UsersPage } from "./pages/admin/UsersPage";
import { HomePage } from "./pages/HomePage";
import { NotificationsPage } from "./pages/NotificationsPage";
import { PhonebookPage } from "./pages/PhonebookPage";
import { ProfilePage } from "./pages/ProfilePage";
import { ApprovalsPage, MyAccessPage } from "./pages/access/AccessPages";
import { AccessResourcesPage, DirectoryAdminPage } from "./pages/admin/DirectoryPages";
import { ComputersPage, DevicePage } from "./pages/remote/RemotePages";
import { RemoteAdminPage } from "./pages/admin/RemoteAdminPage";

const rootRoute = createRootRoute({ component: Layout, notFoundComponent: NotFound });
// Нужное разрешение по пути: разделы настроек — из меню настроек, остальное — здесь.
const PERMS: Record<string, string> = {
  ...Object.fromEntries(adminGroups.flatMap((g) => g.items.map((i) => [i.to, i.perm]))),
  "/desk": "helpdesk.ticket.work",
  "/assets": "helpdesk.ticket.work",
  "/reports": "helpdesk.reports",
  "/computers": "remote.device.view",
  "/computers/$id": "remote.device.view",
};
const allowed = (perm: string, Page: () => React.ReactNode) =>
  function Allowed() {
    return (
      <PermGate perm={perm}>
        <Page />
      </PermGate>
    );
  };
const page = <P extends string>(path: P, component: () => React.ReactNode) =>
  createRoute({ getParentRoute: () => rootRoute, path, component: PERMS[path] ? allowed(PERMS[path], component) : component });
/** Страница модуля: при выключенном модуле — «не найдено». */
const only = (module: ModuleKey, Page: () => React.ReactNode) =>
  function Gated() {
    return (
      <ModuleGate module={module}>
        <Page />
      </ModuleGate>
    );
  };

const kbNew = createRoute({
  getParentRoute: () => rootRoute,
  path: "/kb/new",
  component: only("helpdesk", KbNewPage),
  validateSearch: (s: Record<string, unknown>): { ticket?: number } => {
    const n = Number(s.ticket);
    return Number.isInteger(n) && n > 0 ? { ticket: n } : {};
  },
});

const routeTree = rootRoute.addChildren([
  page("/", HomePage),
  page("/phonebook", only("phonebook", PhonebookPage)),
  page("/tickets", only("helpdesk", MyTicketsPage)),
  page("/tickets/new", only("helpdesk", NewTicketPage)),
  page("/tickets/$number", only("helpdesk", TicketPage)),
  page("/desk", only("helpdesk", DeskPage)),
  page("/ticket-response/$token", only("helpdesk", TicketResponsePage)),
  page("/notifications", NotificationsPage),
  page("/profile", ProfilePage),
  page("/access", only("helpdesk", MyAccessPage)),
  page("/approvals", only("helpdesk", ApprovalsPage)),
  page("/kb", only("helpdesk", KbPage)),
  kbNew,
  page("/kb/$id", only("helpdesk", KbArticlePage)),
  page("/kb/$id/edit", only("helpdesk", KbEditPage)),
  page("/assets", only("helpdesk", AssetsPage)),
  page("/objects/$kind/$id", only("helpdesk", ObjectPage)),
  page("/reports", only("helpdesk", ReportsPage)),
  page("/computers", only("remote", ComputersPage)),
  page("/computers/$id", only("remote", DevicePage)),
  page("/admin/sla", only("helpdesk", SlaAdminPage)),
  page("/admin/automation", only("helpdesk", AutomationAdminPage)),
  page("/admin/templates", only("helpdesk", TemplatesAdminPage)),
  page("/admin/asset-types", only("helpdesk", AssetTypesAdminPage)),
  page("/admin/integrations", only("helpdesk", IntegrationsPage)),
  page("/admin/remote", only("remote", RemoteAdminPage)),
  page("/admin", AdminHomePage),
  page("/admin/ad", AdPage),
  page("/admin/directory", only("directory", DirectoryAdminPage)),
  page("/admin/access-resources", only("helpdesk", AccessResourcesPage)),
  page("/admin/helpdesk", only("helpdesk", HelpdeskAdminPage)),
  page("/admin/users", UsersPage),
  page("/admin/roles", RolesPage),
  page("/admin/sites", SitesPage),
  page("/admin/phonebook", only("phonebook", PhonebookAdminPage)),
  page("/admin/linking", LinkingPage),
  page("/admin/mail", MailPage),
  page("/admin/telephony", only("telephony", TelephonyPage)),
  page("/admin/security", SecurityPage),
  page("/admin/audit", AuditPage),
  page("/admin/system", SystemPage),
]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
