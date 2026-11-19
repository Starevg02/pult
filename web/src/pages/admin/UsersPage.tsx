// Админка → Пользователи и роли: роли пользователей и матрица разрешений.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type RolesMatrix, type UserInfo } from "../../api/client";
import { Badge, Button, Card, Dialog, ErrorBox, Input, PageHeader, Spinner } from "../../ui";
import { formatDate } from "../../format";
import { useMe } from "../../session";

function useRoles() {
  return useQuery({ queryKey: ["roles"], queryFn: () => ok<RolesMatrix>(api.GET("/api/admin/roles")) });
}

export function UsersPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [q, setQ] = useState("");
  const [debounced, setDebounced] = useState("");
  const [editing, setEditing] = useState<UserInfo | null>(null);
  useEffect(() => {
    const h = setTimeout(() => setDebounced(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const users = useQuery({
    queryKey: ["users", debounced],
    queryFn: () => ok<UserInfo[]>(api.GET("/api/admin/users", { params: { query: { q: debounced || undefined } } })),
  });
  const roles = useRoles();
  const isSuper = useMe().data?.is_superadmin === true;
  // Потерян телефон: второй фактор сбрасывается, все сеансы пользователя закрываются.
  const resetTotp = useMutation({
    meta: { toast: t("admin.users.totpResetDone") },
    mutationFn: (id: string) => ok(api.DELETE("/api/admin/users/{id}/totp", { params: { path: { id } } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["users"] }),
  });
  const roleName = (code: string) => roles.data?.roles.find((r) => r.code === code)?.name ?? code;
  return (
    <div className="mx-auto max-w-5xl">
      <PageHeader title={t("admin.users.title")}>
        <Input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("admin.users.search")} className="w-72" />
      </PageHeader>
      <p className="mb-3 text-sm text-muted">{t("admin.users.hint")}</p>
      {users.error && <ErrorBox error={users.error} />}
      {resetTotp.error && <ErrorBox error={resetTotp.error} />}
      {users.isPending ? (
        <Spinner />
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-line text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2 font-medium">{t("admin.users.name")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.users.login")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.users.roles")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.users.lastLogin")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {users.data?.map((u) => (
                <tr key={u.id} className="border-b border-line last:border-0">
                  <td className="px-3 py-2">
                    {u.name} {!u.is_active && <Badge tone="bad">{t("admin.users.inactive")}</Badge>}{" "}
                    {u.totp && <Badge tone="ok">{t("admin.users.totp")}</Badge>}
                  </td>
                  <td className="px-3 py-2 font-mono text-xs">{u.login}</td>
                  <td className="px-3 py-2">
                    <div className="flex flex-wrap gap-1">
                      {u.roles.map((r) => (
                        <Badge key={r} tone={r === "superadmin" ? "accent" : "muted"}>
                          {roleName(r)}
                        </Badge>
                      ))}
                    </div>
                  </td>
                  <td className="px-3 py-2 text-xs text-muted">{formatDate(u.last_login_at, i18n.language)}</td>
                  <td className="px-3 py-2 text-right whitespace-nowrap">
                    {isSuper && u.totp && (
                      <Button
                        variant="ghost"
                        disabled={resetTotp.isPending}
                        onClick={() => window.confirm(t("admin.users.totpResetConfirm", { name: u.name })) && resetTotp.mutate(u.id)}
                      >
                        {t("admin.users.totpReset")}
                      </Button>
                    )}
                    <Button variant="ghost" onClick={() => setEditing(u)}>
                      {t("common.edit")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
      {editing && roles.data && (
        <UserRolesDialog
          user={editing}
          matrix={roles.data}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            void qc.invalidateQueries({ queryKey: ["users"] });
            void qc.invalidateQueries({ queryKey: ["roles"] });
            // Роли могли смениться у себя — меню и права сразу, а не через минуту.
            void qc.invalidateQueries({ queryKey: ["me"] });
          }}
        />
      )}
    </div>
  );
}

function UserRolesDialog({ user, matrix, onClose, onSaved }: { user: UserInfo; matrix: RolesMatrix; onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation();
  const [roles, setRoles] = useState<string[]>(user.roles);
  const save = useMutation({
    mutationFn: () => ok(api.PUT("/api/admin/users/{id}/roles", { params: { path: { id: user.id } }, body: { roles } })),
    onSuccess: onSaved,
  });
  return (
    <Dialog open onClose={onClose} title={t("admin.users.rolesOf", { name: user.name })}>
      <p className="mb-3 text-sm text-muted">{t("admin.users.employeeAlways")}</p>
      <div className="space-y-2">
        {matrix.roles
          .filter((r) => r.code !== "employee")
          .map((r) => (
            <label key={r.code} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={roles.includes(r.code)}
                onChange={(e) => setRoles(e.target.checked ? [...roles, r.code] : roles.filter((x) => x !== r.code))}
              />
              {r.name}
            </label>
          ))}
      </div>
      {save.error && (
        <div className="mt-3">
          <ErrorBox error={save.error} />
        </div>
      )}
      <div className="mt-4 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
          {t("common.save")}
        </Button>
      </div>
    </Dialog>
  );
}

export function RolesPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useRoles();
  const [draft, setDraft] = useState<Record<string, string[]>>({});
  const save = useMutation({
    mutationFn: (code: string) => ok(api.PUT("/api/admin/roles/{code}/permissions", { params: { path: { code } }, body: { permissions: draft[code] ?? [] } })),
    onSuccess: (_, code) => {
      setDraft((d) => Object.fromEntries(Object.entries(d).filter(([k]) => k !== code)));
      void qc.invalidateQueries({ queryKey: ["roles"] });
      void qc.invalidateQueries({ queryKey: ["me"] });
    },
  });
  if (q.isPending) return <Spinner />;
  if (q.error) return <ErrorBox error={q.error} />;
  const roles = q.data.roles.filter((r) => r.code !== "superadmin");
  const has = (code: string, p: string) => (draft[code] ?? q.data.roles.find((r) => r.code === code)?.permissions ?? []).includes(p);
  const toggle = (code: string, p: string) => {
    const cur = draft[code] ?? q.data.roles.find((r) => r.code === code)?.permissions ?? [];
    setDraft({ ...draft, [code]: cur.includes(p) ? cur.filter((x) => x !== p) : [...cur, p] });
  };
  return (
    <div className="mx-auto max-w-6xl">
      <PageHeader title={t("admin.roles.title")} />
      <p className="mb-3 text-sm text-muted">{t("admin.roles.hint")}</p>
      <Card className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="border-b border-line text-left text-xs text-muted">
            <tr>
              <th className="px-3 py-2 font-medium">{t("admin.roles.permission")}</th>
              {roles.map((r) => (
                <th key={r.code} className="px-3 py-2 text-center font-medium">
                  {r.name}
                  <div className="font-normal">{t("admin.roles.users", { n: r.users })}</div>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {q.data.permissions.map((p) => (
              <tr key={p.code} className="border-b border-line last:border-0">
                <td className="px-3 py-2">
                  <div>{p.description}</div>
                  <div className="font-mono text-xs text-muted">{p.code}</div>
                </td>
                {roles.map((r) => (
                  <td key={r.code} className="px-3 py-2 text-center">
                    <input type="checkbox" aria-label={`${r.name}: ${p.code}`} checked={has(r.code, p.code)} onChange={() => toggle(r.code, p.code)} />
                  </td>
                ))}
              </tr>
            ))}
            <tr>
              <td />
              {roles.map((r) => (
                <td key={r.code} className="px-3 py-2 text-center">
                  {draft[r.code] && (
                    <Button variant="primary" onClick={() => save.mutate(r.code)} disabled={save.isPending}>
                      {t("common.save")}
                    </Button>
                  )}
                </td>
              ))}
            </tr>
          </tbody>
        </table>
      </Card>
      {save.error && (
        <div className="mt-3">
          <ErrorBox error={save.error} />
        </div>
      )}
    </div>
  );
}
