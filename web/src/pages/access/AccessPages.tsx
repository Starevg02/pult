// «Мои доступы» и «Согласования»: что запрошено и решения владельцев ресурсов.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AccessRequestView, type ChangeApprovalItem } from "../../api/client";
import { formatDate } from "../../format";
import { Badge, Button, Card, Empty, ErrorBox, Input, PageHeader, Spinner } from "../../ui";
import { accessTone } from "../helpdesk/directoryUtil";
import { dyn } from "../helpdesk/helpdeskUtil";
import { TicketLink } from "../helpdesk/parts";
import { ChangeApprovals } from "../helpdesk/ItilPages";

function Status({ r }: { r: AccessRequestView }) {
  const { t } = useTranslation();
  return <Badge tone={accessTone[r.status] ?? "muted"}>{dyn(t, `access.status.${r.status}`)}</Badge>;
}

export function MyAccessPage() {
  const { t, i18n } = useTranslation();
  const q = useQuery({ queryKey: ["access-mine"], queryFn: () => ok<AccessRequestView[]>(api.GET("/api/helpdesk/access/mine")) });
  return (
    <div className="mx-auto max-w-4xl">
      <PageHeader title={t("access.mine")}>
        <Link to="/tickets/new">
          <Button variant="primary">{t("access.request")}</Button>
        </Link>
      </PageHeader>
      {q.error && <ErrorBox error={q.error} onRetry={() => void q.refetch()} />}
      {q.isPending && <Spinner />}
      {q.data && q.data.length === 0 && <Empty>{t("access.mineEmpty")}</Empty>}
      {q.data && q.data.length > 0 && (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-line text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2 font-medium">{t("access.resource")}</th>
                <th className="px-3 py-2 font-medium">{t("access.ticket")}</th>
                <th className="px-3 py-2 font-medium">{t("access.statusLabel")}</th>
                <th className="px-3 py-2 font-medium">{t("access.approver")}</th>
                <th className="px-3 py-2 font-medium">{t("access.granted")}</th>
              </tr>
            </thead>
            <tbody>
              {q.data.map((r) => (
                <tr key={r.id} className="border-b border-line last:border-0">
                  <td className="px-3 py-2">
                    <div className="font-medium">{r.resource_name}</div>
                    <div className="text-xs text-muted">{dyn(t, `access.kind.${r.resource_kind}`)}</div>
                  </td>
                  <td className="px-3 py-2">
                    <TicketLink number={r.ticket_number} className="font-mono text-xs text-accent">
                      #{r.ticket_number}
                    </TicketLink>
                  </td>
                  <td className="px-3 py-2">
                    <Status r={r} />
                    {r.decision_comment && <div className="mt-1 text-xs text-muted">{r.decision_comment}</div>}
                  </td>
                  <td className="px-3 py-2">{r.approver_name ?? "—"}</td>
                  <td className="px-3 py-2 whitespace-nowrap">{r.granted_at ? formatDate(r.granted_at, i18n.language) : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}

export function ApprovalsPage() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["approvals"], queryFn: () => ok<AccessRequestView[]>(api.GET("/api/helpdesk/approvals")) });
  const pending = q.data?.filter((r) => r.status === "pending") ?? [];
  const decided = q.data?.filter((r) => r.status !== "pending") ?? [];
  const changes = useQuery({ queryKey: ["change-approvals"], queryFn: () => ok<ChangeApprovalItem[]>(api.GET("/api/helpdesk/change-approvals")) });
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("access.approvals")} />
      {q.error && <ErrorBox error={q.error} onRetry={() => void q.refetch()} />}
      {q.isPending && <Spinner />}
      {q.data && pending.length === 0 && changes.data?.every((c) => c.status !== "pending") && <Empty>{t("access.approvalsEmpty")}</Empty>}
      <ChangeApprovals />
      {pending.map((r) => (
        <ApprovalCard key={r.id} r={r} />
      ))}
      {decided.length > 0 && (
        <section>
          <h2 className="mb-2 text-sm font-semibold text-muted">{t("access.decidedByMe")}</h2>
          <Card>
            <ul className="divide-y divide-line text-sm">
              {decided.map((r) => (
                <li key={r.id} className="flex flex-wrap items-center gap-2 px-3 py-2">
                  <TicketLink number={r.ticket_number} className="font-mono text-xs text-accent">
                    #{r.ticket_number}
                  </TicketLink>
                  <span className="min-w-0 flex-1 truncate">
                    {r.resource_name} · {r.person_name ?? "—"}
                  </span>
                  <Status r={r} />
                </li>
              ))}
            </ul>
          </Card>
        </section>
      )}
    </div>
  );
}

function ApprovalCard({ r }: { r: AccessRequestView }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [comment, setComment] = useState("");
  const decide = useMutation({
    mutationFn: (approve: boolean) => ok(api.POST("/api/helpdesk/approvals/{id}", { params: { path: { id: r.id } }, body: { approve, comment: comment.trim() || null } })),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["approvals"] });
      void qc.invalidateQueries({ queryKey: ["notifications"] });
    },
  });
  return (
    <Card className="p-4">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{r.resource_name}</span>
        <span className="text-xs text-muted">{dyn(t, `access.kind.${r.resource_kind}`)}</span>
        <span className="ml-auto">
          <TicketLink number={r.ticket_number} className="font-mono text-xs text-accent">
            #{r.ticket_number}
          </TicketLink>
        </span>
      </div>
      <div className="mt-1 text-sm">
        {t("access.forPerson")}: <span className="font-medium">{r.person_name ?? "—"}</span>
      </div>
      <div className="text-sm text-muted">{r.ticket_title}</div>
      {r.sent_at && (
        <div className="text-xs text-muted">
          {t("access.sentAt")}: {formatDate(r.sent_at, i18n.language)}
        </div>
      )}
      <div className="mt-3 flex flex-wrap gap-2">
        <Input className="min-w-48 flex-1" value={comment} maxLength={2000} onChange={(e) => setComment(e.target.value)} placeholder={t("access.comment")} />
        <Button variant="primary" onClick={() => decide.mutate(true)} disabled={decide.isPending}>
          {t("access.approve")}
        </Button>
        <Button variant="danger" onClick={() => decide.mutate(false)} disabled={decide.isPending}>
          {t("access.reject")}
        </Button>
      </div>
      {decide.error && <ErrorBox error={decide.error} />}
    </Card>
  );
}
