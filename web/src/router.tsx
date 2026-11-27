import { createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router";
import { AssetsPage, MyAssetsPage, ObjectPage } from "./pages/assets/AssetPages";
import { LicensesPage } from "./pages/assets/CmdbParts";
import { KbArticlePage, KbEditPage, KbNewPage, KbPage } from "./pages/kb/KbPages";
import { CatalogPage } from "./pages/helpdesk/CatalogPage";
import { ReportsPage } from "./pages/reports/ReportsPage";
import { AssetTypesAdminPage, AutomationAdminPage, IntegrationsPage, SlaAdminPage, TemplatesAdminPage } from "./pages/admin/ServicePages";
import { Layout, ModuleGate, NotFound, PageCrash, PermGate } from "./layout";
import { adminGroups } from "./pages/admin/adminMenu";
import type { ModuleKey } from "./session";
import { AdPage } from "./pages/admin/AdPage";
import { AdminHomePage } from "./pages/admin/AdminHomePage";
import { HelpdeskAdminPage } from "./pages/admin/HelpdeskAdminPage";
import { NewTicketPage } from "./pages/helpdesk/NewTicketPage";
import { TicketResponsePage } from "./pages/helpdesk/PublicPages";
import { TicketPage } from "./pages/helpdesk/TicketPage";
import { DeskPage, MyTicketsPage } from "./pages/helpdesk/TicketsPage";
import { ChangeRulesPage, ChangesPage, ProblemsPage } from "./pages/helpdesk/ItilPages";
import { VaultPage } from "./pages/helpdesk/VaultPage";
import { AssistantPage } from "./pages/helpdesk/assist";
import { MailPage } from "./pages/admin/MailPage";
import { LinkingPage, PhonebookAdminPage } from "./pages/admin/PhonebookAdminPage";
import { AuditPage, SecurityPage, SystemPage, TelephonyPage } from "./pages/admin/SettingsPages";
import { ChatPage, ChatsPage } from "./pages/chat/ChatPages";
import { ChatAdminPage } from "./pages/chat/ChatAdminPage";
import { ExtensionsPage } from "./pages/admin/ExtensionsPage";
import { SitesPage } from "./pages/admin/SitesPage";
import { RolesPage, UsersPage } from "./pages/admin/UsersPage";
import { HomePage } from "./pages/HomePage";
import { NotificationsPage } from "./pages/NotificationsPage";
import { PhonebookPage } from "./pages/PhonebookPage";
import { ProfilePage } from "./pages/ProfilePage";
import { UserPage } from "./pages/people/UserPage";
import { FeedPage } from "./pages/people/FeedPage";
import { ApprovalsPage, MyAccessPage } from "./pages/access/AccessPages";
import { AccessResourcesPage, DirectoryAdminPage } from "./pages/admin/DirectoryPages";
import { ComputersPage, DevicePage } from "./pages/remote/RemotePages";
import { RemoteAdminPage } from "./pages/admin/RemoteAdminPage";

const rootRoute = createRootRoute({ component: Layout, notFoundComponent: NotFound });
// Нужное разрешение по пути: разделы настроек — из меню настроек, остальное — здесь.
const PERMS: Record<string, string> = {
  ...Object.fromEntries(adminGroups.flatMap((g) => g.items.map((i) => [i.to, i.perm]))),
  "/desk": "helpdesk.ticket.work",
  "/problems": "helpdesk.ticket.work",
  "/changes": "helpdesk.ticket.work",
  "/vault": "helpdesk.vault.use",
  "/assistant": "helpdesk.assist.use",
  "/assets": "helpdesk.ticket.work",
  "/licenses": "helpdesk.ticket.work",
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

const ticketsNew = createRoute({
  getParentRoute: () => rootRoute,
  path: "/tickets/new",
  component: only("helpdesk", NewTicketPage),
  // Категория из каталога услуг: форма открывается сразу с ней и её полями.
  validateSearch: (s: Record<string, unknown>): { category?: string } =>
    typeof s.category === "string" && /^[0-9a-f-]{36}$/i.test(s.category) ? { category: s.category } : {},
});

const routeTree = rootRoute.addChildren([
  page("/", HomePage),
  // Форма входа показывается на любом адресе, своей страницы у неё нет: /login из закладки ведёт на главную.
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/login",
    beforeLoad: () => {
      throw redirect({ to: "/", replace: true });
    },
  }),
  page("/phonebook", only("phonebook", PhonebookPage)),
  page("/people/$id", UserPage),
  page("/feed", FeedPage),
  page("/tickets", only("helpdesk", MyTicketsPage)),
  ticketsNew,
  page("/catalog", only("helpdesk", CatalogPage)),
  page("/tickets/$number", only("helpdesk", TicketPage)),
  page("/desk", only("helpdesk", DeskPage)),
  page("/problems", only("helpdesk", ProblemsPage)),
  page("/changes", only("helpdesk", ChangesPage)),
  page("/vault", only("helpdesk", VaultPage)),
  page("/assistant", only("helpdesk", AssistantPage)),
  page("/ticket-response/$token", only("helpdesk", TicketResponsePage)),
  page("/notifications", NotificationsPage),
  page("/chats", only("chat", ChatsPage)),
  page("/chats/$id", only("chat", ChatPage)),
  page("/profile", ProfilePage),
  page("/access", only("helpdesk", MyAccessPage)),
  page("/my-assets", only("helpdesk", MyAssetsPage)),
  page("/approvals", only("helpdesk", ApprovalsPage)),
  page("/kb", only("helpdesk", KbPage)),
  kbNew,
  page("/kb/$id", only("helpdesk", KbArticlePage)),
  page("/kb/$id/edit", only("helpdesk", KbEditPage)),
  page("/assets", only("helpdesk", AssetsPage)),
  page("/licenses", only("helpdesk", LicensesPage)),
  page("/objects/$kind/$id", only("helpdesk", ObjectPage)),
  page("/reports", only("helpdesk", ReportsPage)),
  page("/computers", only("remote", ComputersPage)),
  page("/computers/$id", only("remote", DevicePage)),
  page("/admin/sla", only("helpdesk", SlaAdminPage)),
  page("/admin/automation", only("helpdesk", AutomationAdminPage)),
  page("/admin/templates", only("helpdesk", TemplatesAdminPage)),
  page("/admin/asset-types", only("helpdesk", AssetTypesAdminPage)),
  page("/admin/integrations", only("helpdesk", IntegrationsPage)),
  page("/admin/change-rules", only("helpdesk", ChangeRulesPage)),
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
  page("/admin/chats", only("chat", ChatAdminPage)),
  page("/admin/extensions", only("helpdesk", ExtensionsPage)),
  page("/admin/security", SecurityPage),
  page("/admin/audit", AuditPage),
  page("/admin/system", SystemPage),
]);

export const router = createRouter({ routeTree, defaultErrorComponent: PageCrash });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
