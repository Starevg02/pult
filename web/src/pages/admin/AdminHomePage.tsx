// Настройки: что где находится и зачем это нужно.
import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { useMe, useOrg } from "../../session";
import { Card, PageHeader } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { adminGroups, visibleItems } from "./adminMenu";

export function AdminHomePage() {
  const { t } = useTranslation();
  const me = useMe().data;
  const modules = useOrg().data?.modules;
  return (
    <div className="mx-auto max-w-4xl space-y-6">
      <PageHeader title={t("nav.settings")} />
      {adminGroups.map((g) => {
        const items = visibleItems(g.items, me, modules);
        if (items.length === 0) return null;
        return (
          <section key={g.key}>
            <h2 className="mb-2 text-sm font-semibold text-muted">{dyn(t, `admin.groups.${g.key}`)}</h2>
            <div className="grid gap-3 sm:grid-cols-2">
              {items.map((i) => (
                <Link key={i.to} to={i.to}>
                  <Card className="h-full p-4 hover:bg-hover">
                    <div className="font-medium">{dyn(t, `admin.menu.${i.key}`)}</div>
                    <div className="mt-1 text-sm text-muted">{dyn(t, `admin.about.${i.key}`)}</div>
                  </Card>
                </Link>
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}
