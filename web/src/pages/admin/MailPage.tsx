// Админка → Почта: SMTP с проверкой и последние письма очереди.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok } from "../../api/client";
import type { components } from "../../api/schema";
import { formatDate } from "../../format";
import { Badge, Button, Card, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";

type MailView = components["schemas"]["MailView"];
type OutboxRow = components["schemas"]["OutboxRow"];

export function MailPage() {
  const q = useQuery({ queryKey: ["mail"], queryFn: () => ok<MailView>(api.GET("/api/admin/mail")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return <MailForm data={q.data} />;
}

function MailForm({ data }: { data: MailView }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data.settings);
  const [password, setPassword] = useState("");
  // Сервер понимает пустой пароль как «удалить сохранённый» (например, ретранслятор без входа).
  const [dropPassword, setDropPassword] = useState(false);
  const [to, setTo] = useState("");
  const outbox = useQuery({ queryKey: ["outbox"], queryFn: () => ok<OutboxRow[]>(api.GET("/api/admin/mail/outbox")), refetchInterval: 30_000 });
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok(api.PUT("/api/admin/mail", { body: { settings: v, password: dropPassword ? "" : password || null } })),
    onSuccess: () => {
      setPassword("");
      setDropPassword(false);
      void qc.invalidateQueries({ queryKey: ["mail"] });
    },
  });
  const test = useMutation({ mutationFn: () => ok(api.POST("/api/admin/mail/test", { body: { to } })) });
  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <PageHeader title={t("admin.mail.title")} />
      <Card className="grid gap-4 p-5 sm:grid-cols-2">
        <label className="flex items-center gap-2 text-sm sm:col-span-2">
          <input type="checkbox" checked={v.enabled} onChange={(e) => setV({ ...v, enabled: e.target.checked })} />
          {t("admin.mail.enabled")}
        </label>
        <Field label={t("admin.mail.host")}>
          <Input value={v.host} onChange={(e) => setV({ ...v, host: e.target.value })} placeholder="smtp.example.local" />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label={t("admin.mail.port")}>
            <Input type="number" value={v.port} onChange={(e) => setV({ ...v, port: Number(e.target.value) })} />
          </Field>
          <Field label={t("admin.mail.security")}>
            <Select value={v.security} onChange={(e) => setV({ ...v, security: e.target.value === "tls" ? "tls" : "starttls" })}>
              <option value="starttls">STARTTLS</option>
              <option value="tls">TLS</option>
            </Select>
          </Field>
        </div>
        <Field label={t("admin.mail.username")} hint={t("admin.mail.usernameHint")}>
          <Input value={v.username} onChange={(e) => setV({ ...v, username: e.target.value })} />
        </Field>
        <Field label={t("admin.mail.password")} hint={data.has_password ? t("admin.ad.passwordKept") : undefined}>
          <Input type="password" autoComplete="new-password" value={password} disabled={dropPassword} onChange={(e) => setPassword(e.target.value)} />
          {data.has_password && (
            <label className="mt-1 flex items-center gap-2 text-xs text-muted">
              <input type="checkbox" checked={dropPassword} onChange={(e) => setDropPassword(e.target.checked)} />
              {t("admin.mail.dropPassword")}
            </label>
          )}
        </Field>
        <Field label={t("admin.mail.from")}>
          <Input value={v.from_address} onChange={(e) => setV({ ...v, from_address: e.target.value })} placeholder="helpdesk@example.local" />
        </Field>
        <Field label={t("admin.mail.fromName")}>
          <Input value={v.from_name} onChange={(e) => setV({ ...v, from_name: e.target.value })} />
        </Field>
        <div className="sm:col-span-2">
          {save.error && <ErrorBox error={save.error} />}
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </Card>
      <Card className="p-5">
        <h2 className="mb-3 font-semibold">{t("admin.mail.test")}</h2>
        <div className="flex gap-2">
          <Input type="email" value={to} onChange={(e) => setTo(e.target.value)} placeholder="ivanov@example.local" />
          <Button onClick={() => test.mutate()} disabled={!to || test.isPending}>
            {t("admin.mail.send")}
          </Button>
        </div>
        {test.data && (
          <p className={`mt-2 text-sm ${test.data.ok ? "text-ok" : "text-bad"}`}>{test.data.ok ? t("admin.mail.sent") : test.data.error}</p>
        )}
        {test.error && <ErrorBox error={test.error} />}
      </Card>
      <Card className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="border-b border-line text-left text-xs text-muted">
            <tr>
              <th className="px-3 py-2 font-medium">{t("admin.mail.created")}</th>
              <th className="px-3 py-2 font-medium">{t("admin.mail.to")}</th>
              <th className="px-3 py-2 font-medium">{t("admin.mail.subject")}</th>
              <th className="px-3 py-2 font-medium">{t("admin.ad.runStatus")}</th>
            </tr>
          </thead>
          <tbody>
            {outbox.data?.map((r) => (
              <tr key={r.id} className="border-b border-line last:border-0">
                <td className="px-3 py-1.5 whitespace-nowrap">{formatDate(r.created_at, i18n.language)}</td>
                <td className="px-3 py-1.5">{r.to_addrs.join(", ")}</td>
                <td className="px-3 py-1.5">{r.subject}</td>
                <td className="px-3 py-1.5">
                  <Badge tone={r.status === "sent" ? "ok" : r.status === "failed" ? "bad" : "muted"}>{r.status}</Badge>
                  {r.last_error && <div className="text-xs text-bad">{r.last_error}</div>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>
    </div>
  );
}
