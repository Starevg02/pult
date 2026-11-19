import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { api, type Health, type NegotiateError } from "../api/client";

/** Состояние системы по `/health`: 200 — всё в порядке, 503 — тело то же, но есть недоступные компоненты. */
async function fetchHealth(): Promise<Health> {
  const { data, error } = await api.GET("/health");
  const health = data ?? error;
  if (!health) throw new Error("no body");
  return health;
}

/** Прототип П4: браузер сам отвечает на `WWW-Authenticate: Negotiate` билетом Kerberos. */
type DomainLogin = { principal: string } | { error: NegotiateError["error"] | "unreachable" };

async function fetchDomainLogin(): Promise<DomainLogin> {
  const { data, error } = await api.GET("/api/auth/negotiate");
  if (data) return { principal: data.principal };
  return { error: error?.error ?? "unreachable" };
}

export function StatusPage() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["health"], queryFn: fetchHealth, refetchInterval: 30_000 });
  // Один раз при открытии: каждый отказ пишется в журнал сервера.
  const login = useQuery({ queryKey: ["domain-login"], queryFn: fetchDomainLogin, retry: false, staleTime: Infinity });

  return (
    <section className="mx-auto max-w-xl">
      <div className="mb-4 flex items-center justify-between">
        <h1 className="text-xl font-semibold">{t("status.title")}</h1>
        <button
          type="button"
          onClick={() => void q.refetch()}
          className="rounded-md border border-line-strong bg-paper px-3 py-1.5 text-sm hover:bg-hover"
        >
          {t("status.refresh")}
        </button>
      </div>

      <div className="rounded-lg border border-line bg-paper">
        {q.isPending && <p className="p-4 text-muted">{t("status.loading")}</p>}
        {q.isError && <p className="p-4 text-bad">{t("status.unreachable")}</p>}
        {q.data && (
          <dl className="divide-y divide-line">
            <Row label={t("status.overall")}>
              <span className={q.data.status === "ok" ? "text-ok" : "text-bad"}>
                {q.data.status === "ok" ? t("status.state.ok") : t("status.state.degraded")}
              </span>
            </Row>
            <Row label={t("status.database")}>
              <span className={q.data.database === "up" ? "text-ok" : "text-bad"}>
                {t(`status.check.${q.data.database}`)}
              </span>
            </Row>
            <Row label={t("status.version")}>
              <span className="font-mono text-sm">{q.data.version}</span>
            </Row>
            <Row label={t("status.domainLogin")}>
              {!login.data ? (
                <span className="text-muted">{t("status.loading")}</span>
              ) : "principal" in login.data ? (
                <span className="font-mono text-sm text-ok">{login.data.principal}</span>
              ) : (
                <span className="text-sm text-muted" title={login.data.error}>
                  {t(`status.domain.${login.data.error}`)}
                </span>
              )}
            </Row>
          </dl>
        )}
      </div>
    </section>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between px-4 py-3">
      <dt className="text-muted">{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}
