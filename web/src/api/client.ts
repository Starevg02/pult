// Клиент API, типизированный по схеме OpenAPI сервера (schema.ts генерируется: `npm run gen:api`).
// Пути, параметры и ответы проверяет компилятор: изменился API на сервере — не соберётся фронтенд.
import createClient, { type Middleware } from "openapi-fetch";
import type { components, paths } from "./schema";

/** CSRF-токен сеанса: cookie `__Host-pult_csrf` (или `pult_csrf` в разработке по HTTP). */
export function csrfToken(): string | null {
  for (const part of document.cookie.split(";")) {
    const [k, ...v] = part.trim().split("=");
    if (k === "__Host-pult_csrf" || k === "pult_csrf") return v.join("=");
  }
  return null;
}

/** Ошибка API: код для программы, текст для человека. */
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
    public details?: unknown,
  ) {
    super(message);
  }
}

type ErrorBody = components["schemas"]["ErrorBody"];

const csrf: Middleware = {
  onRequest({ request }) {
    if (!["GET", "HEAD", "OPTIONS"].includes(request.method)) {
      const t = csrfToken();
      if (t) request.headers.set("X-CSRF-Token", t);
    }
    return request;
  },
};

export const api = createClient<paths>({ baseUrl: "/", credentials: "same-origin" });
api.use(csrf);

/** Ответ или исключение `ApiError`: для `useQuery`/`useMutation`. */
export async function ok<T>(p: Promise<{ data?: T; error?: unknown; response: Response }>): Promise<T> {
  const { data, error, response } = await p;
  if (response.ok) return data as T;
  const body = (error ?? {}) as Partial<ErrorBody>;
  throw new ApiError(response.status, body.error ?? `http_${response.status}`, body.message ?? response.statusText, body.details);
}

export type Health = components["schemas"]["Health"];
export type NegotiateError = components["schemas"]["NegotiateError"];
export type Me = components["schemas"]["Me"];
export type UserPrefs = components["schemas"]["UserPrefs"];
export type Profile = components["schemas"]["Profile"];
export type MySession = components["schemas"]["MySession"];
export type Revoked = components["schemas"]["Revoked"];
export type AuthContext = components["schemas"]["AuthContext"];
export type TotpStatus = components["schemas"]["TotpStatus"];
export type TotpSetup = components["schemas"]["TotpSetup"];
export type LocaleSettings = components["schemas"]["LocaleSettings"];
export type Person = components["schemas"]["Person"];
export type PersonInput = components["schemas"]["PersonInput"];
export type DictItem = components["schemas"]["DictItem"];
export type Duplicate = components["schemas"]["Duplicate"];
export type PhonebookConfig = components["schemas"]["PhonebookConfig"];
export type FieldSource = components["schemas"]["FieldSource"];
export type Site = components["schemas"]["Site"];
export type SiteInput = components["schemas"]["SiteInput"];
export type Engineer = components["schemas"]["Engineer"];
export type UserInfo = components["schemas"]["UserInfo"];
export type RolesMatrix = components["schemas"]["RolesMatrix"];
export type AuditRow = components["schemas"]["AuditRow"];
export type VerifyResult = components["schemas"]["VerifyResult"];
export type SecuritySettings = components["schemas"]["SecuritySettings"];
export type AdOverview = components["schemas"]["AdOverview"];
export type DomainInput = components["schemas"]["DomainInput"];
export type DcCheckView = components["schemas"]["DcCheckView"];
export type TicketRow = components["schemas"]["TicketRow"];
export type TicketDetail = components["schemas"]["TicketDetail"];
export type TicketHead = components["schemas"]["TicketHead"];
export type FeedItem = components["schemas"]["FeedItem"];
export type AttachmentView = components["schemas"]["AttachmentView"];
export type Category = components["schemas"]["Category"];
export type Created = components["schemas"]["Created"];
export type TicketContext = components["schemas"]["TicketContext"];
export type ComputerRow = components["schemas"]["ComputerRow"];
export type EngineerView = components["schemas"]["EngineerView"];
export type AnnouncementView = components["schemas"]["AnnouncementView"];
export type HelpdeskSettings = components["schemas"]["HelpdeskSettings"];
export type Absence = components["schemas"]["Absence"];
export type TokenInfo = components["schemas"]["TokenInfo"];
export type BackupStatus = components["schemas"]["BackupStatus"];
export type EngineerCandidate = components["schemas"]["EngineerCandidate"];
export type SetupDomain = components["schemas"]["SetupDomain"];
export type SetupStatus = components["schemas"]["SetupStatus"];
export type OrgSettings = components["schemas"]["OrgSettings"];
export type Modules = components["schemas"]["Modules"];
export type InstallLanguageResult = components["schemas"]["InstallLanguageResult"];
export type TlsStatus = components["schemas"]["TlsStatus"];
export type Summary = components["schemas"]["Summary"];
export type LinkCandidate = components["schemas"]["LinkCandidate"];
export type LinkSettings = components["schemas"]["LinkSettings"];
export type TelephonyView = components["schemas"]["TelephonyView"];
export type NotificationList = components["schemas"]["NotificationList"];
export type ImportRun = components["schemas"]["ImportRun"];
export type CloseCode = components["schemas"]["CloseCode"];
export type FoundObject = components["schemas"]["FoundObject"];
export type LinkedObject = components["schemas"]["LinkedObject"];
export type Group = components["schemas"]["Group"];
export type ResponseTemplate = components["schemas"]["ResponseTemplate"];
export type BulkResult = components["schemas"]["BulkResult"];
export type TicketTemplate = components["schemas"]["TicketTemplate"];
export type Article = components["schemas"]["Article"];
export type ArticleRow = components["schemas"]["ArticleRow"];
export type KbSection = components["schemas"]["Section"];
export type Asset = components["schemas"]["Asset"];
export type AssetType = components["schemas"]["AssetType"];
export type ObjectCard = components["schemas"]["ObjectCard"];
export type Report = components["schemas"]["Report"];
export type Bucket = components["schemas"]["Bucket"];
export type Calendar = components["schemas"]["Calendar"];
export type SpecialDay = components["schemas"]["SpecialDay"];
export type SlaPolicy = components["schemas"]["SlaPolicy"];
export type Rule = components["schemas"]["Rule"];
export type Recurring = components["schemas"]["Recurring"];
export type ApiToken = components["schemas"]["ApiToken"];
export type CreatedToken = components["schemas"]["CreatedToken"];
export type Webhook = components["schemas"]["Webhook"];
export type AccessRequestView = components["schemas"]["AccessRequestView"];
export type ResourceView = components["schemas"]["ResourceView"];
export type ResourceInput = components["schemas"]["ResourceInput"];
export type OperationView = components["schemas"]["OperationView"];
export type OperationKind = components["schemas"]["OperationKind"];
export type AccountView = components["schemas"]["AccountView"];
export type DirectoryConfig = components["schemas"]["DirectoryConfig"];
export type DeviceRow = components["schemas"]["DeviceRow"];
export type DeviceCard = components["schemas"]["DeviceCard"];
export type SessionRow = components["schemas"]["SessionRow"];
export type NewConnection = components["schemas"]["NewConnection"];
export type IssuedConnection = components["schemas"]["IssuedConnection"];
export type ConnectionView = components["schemas"]["ConnectionView"];
export type RemoteAction = components["schemas"]["RemoteAction"];
export type ShadowMode = components["schemas"]["ShadowMode"];
export type RemoteConnectionRow = components["schemas"]["RemoteConnectionRow"];
export type EnrollmentTokenRow = components["schemas"]["EnrollmentTokenRow"];
export type CreatedEnrollmentToken = components["schemas"]["CreatedEnrollmentToken"];
export type RemotePolicyRow = components["schemas"]["RemotePolicyRow"];
export type PolicyImportResult = components["schemas"]["PolicyImportResult"];
export type AgentPackageRow = components["schemas"]["AgentPackageRow"];
export type RemoteSettings = components["schemas"]["RemoteSettings"];
