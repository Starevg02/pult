// Разделы настроек: группы, нужные разрешения и модуль (выключенный в мастере модуль прячет раздел).
import type { Me, Modules } from "../../api/client";
import { can } from "../../session";

type AdminItem = { to: string; key: string; perm: string; module?: keyof Modules };

/** Разделы группы, доступные пользователю при включённых модулях. */
export function visibleItems(items: readonly AdminItem[], me: Me | undefined, modules: Modules | undefined): AdminItem[] {
  return items.filter((i) => can(me, i.perm) && (!i.module || (modules?.[i.module] ?? true)));
}

export const adminGroups = [
  {
    key: "work",
    items: [
      { to: "/admin/sites", key: "sites", perm: "people.sites.manage" },
      { to: "/admin/helpdesk", key: "helpdesk", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/sla", key: "sla", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/automation", key: "automation", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/templates", key: "templates", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/asset-types", key: "assetTypes", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/change-rules", key: "changeRules", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/mail", key: "mail", perm: "core.mail.manage" },
      { to: "/admin/access-resources", key: "accessResources", perm: "helpdesk.access.resources", module: "helpdesk" },
    ],
  },
  {
    key: "people",
    items: [
      { to: "/admin/users", key: "users", perm: "core.roles.manage" },
      { to: "/admin/roles", key: "roles", perm: "core.roles.manage" },
      { to: "/admin/phonebook", key: "phonebook", perm: "phonebook.edit", module: "phonebook" },
      { to: "/admin/linking", key: "linking", perm: "people.link.manage" },
    ],
  },
  {
    key: "connections",
    items: [
      { to: "/admin/ad", key: "ad", perm: "people.ad.manage" },
      { to: "/admin/directory", key: "directory", perm: "directory.manage", module: "directory" },
      { to: "/admin/telephony", key: "telephony", perm: "phonebook.admin", module: "telephony" },
      { to: "/admin/integrations", key: "integrations", perm: "helpdesk.admin", module: "helpdesk" },
      { to: "/admin/remote", key: "remote", perm: "remote.manage", module: "remote" },
      // Такого разрешения нет ни у одной роли: раздел виден только суперадминистратору (как и его API).
      { to: "/admin/chats", key: "chats", perm: "core.superadmin", module: "chat" },
      { to: "/admin/extensions", key: "extensions", perm: "core.superadmin", module: "helpdesk" },
    ],
  },
  {
    key: "system",
    items: [
      { to: "/admin/security", key: "security", perm: "core.settings.manage" },
      { to: "/admin/audit", key: "audit", perm: "core.audit.view" },
      { to: "/admin/system", key: "system", perm: "core.settings.manage" },
    ],
  },
] as const;
