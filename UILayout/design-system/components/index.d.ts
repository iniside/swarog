// Swarog Admin primitives. Every function returns an HTML string; insert it with innerHTML.

/** Annotation tag carried by every contributed fragment. */
export interface Contribution { module: string; point: string; prim?: string }
export type PanelState = 'ok' | 'loading' | 'empty' | 'stale' | 'error' | 'denied';
export type Tone = '' | 'ok' | 'warn' | 'err' | 'info' | 'accent';

export interface PanelProps {
  c?: Contribution;
  title?: string; meta?: string; icon?: string;
  tools?: string | ((o: { readonly: boolean }) => string);
  body?: string | ((o: { readonly: boolean }) => string);
  foot?: string; banner?: string; flush?: boolean; cls?: string; style?: string;
  state?: PanelState; readonly?: boolean; noForce?: boolean;
  emptyTitle?: string; emptyDetail?: string; emptyIcon?: string; emptyActions?: string;
  errorDetail?: string; staleText?: string; readonlyText?: string; perm?: string; skeletonRows?: number;
}
export declare function Panel(props: PanelProps): string;

export interface Column { label: string; num?: boolean; wrap?: boolean; sorted?: 'asc' | 'desc' }
export type Row = string[] | { cells: string[]; href?: string; sel?: boolean };
export interface DataTableProps {
  cols: Column[]; rows: Row[];
  filters?: Array<{ k: string; v: string } | string>; search?: boolean; searchPlaceholder?: string; searchWidth?: number; toolbarRight?: string;
  selectable?: boolean; bulk?: string;
  rowMenu?: (row: Row, index: number) => string;
  total?: string; page?: string; footNote?: string; pager?: boolean; cls?: string;
}
export declare function DataTable(props: DataTableProps): string;

export interface KpiTileProps {
  c: Contribution; label: string; value: string; unit?: string; delta?: string; icon?: string;
  spark?: number[]; sparkColor?: string; bar?: number; href?: string; state?: PanelState; perm?: string; noForce?: boolean;
}
export declare function KpiTile(props: KpiTileProps): string;

export declare function PropertyList(pairs: Array<[label: string, valueHtml: string]>): string;

export interface TimelineEntry { t: string; day?: string; icon?: string; tone?: Tone; html: string; meta?: string }
export declare function Timeline(entries: TimelineEntry[]): string;

export interface Field {
  label: string; type?: 'text' | 'select' | 'bool' | 'checks' | 'textarea' | 'user' | 'money'; typeName?: string;
  value?: string | boolean | string[]; options?: string[]; required?: boolean; help?: string; error?: string;
  disabled?: boolean; placeholder?: string; mono?: boolean; w?: string; rows?: number;
}
export interface FormProps { fields: Field[]; submit?: string; note?: string; actions?: boolean; onSubmit?: string }
export declare function Form(props: FormProps, readonly?: boolean): string;

export interface ConfirmActionProps {
  c?: Contribution; title: string; body?: string; impact?: Array<[string, string]>;
  tone?: 'danger' | 'warn' | 'ok'; icon?: string; reason?: string; typeToConfirm?: string;
  confirm?: string; footNote?: string; done?: string; onConfirm?: string; wide?: boolean;
}
/** Opens the modal; returns nothing. */
export declare function ConfirmAction(props: ConfirmActionProps): void;

export interface SecretRevealProps { value: string; title?: string; text?: string; extra?: string }
export declare function SecretReveal(props: SecretRevealProps): string;

export interface Service { name: string; state: 'running' | 'degraded' | 'restarting' | 'crashed' | 'down'; p99: string; note?: string }
export declare function ServiceStatus(items: Service[], opts?: { bars?: boolean }): string;

export interface Series { values: number[]; max: number; color: string; unit: string }
export interface TimeSeriesChartProps { labels: string[]; series: Series[]; right?: boolean; aria?: string }
export declare function TimeSeriesChart(props: TimeSeriesChartProps): string;

export interface MenuItem { label: string; icon?: string; danger?: boolean; disabled?: string; hint?: string; run?: string }
export interface MenuGroup { c?: Contribution; label?: string; note?: string; items: MenuItem[] }
/** Opens a menu registered with defineMenu next to the event's target. */
export declare function MenuAction(ev: Event, menuId: string, arg: string): void;
export declare function defineMenu(menuId: string, build: (arg: string) => MenuGroup[]): void;

export declare function Badge(text: string, tone?: Tone, plain?: boolean): string;
/** The one mapping from a status value to its badge tone. */
export declare function status(value: string): string;

/** Wraps a fragment for the annotation layer (body.annot). */
export declare function Annotation(c: Contribution, html: string, opts?: { inline?: boolean; left?: boolean; tag?: string; cls?: string; style?: string }): string;

export declare function icon(name: string, cls?: string): string;
export declare function ref(kind: string, id: string): string;
export declare function rowMenuBtn(menuId: string, arg: string): string;
export declare function toast(text: string, tone?: 'ok' | 'info'): void;

/** Lowercase names are the same functions as their aliases: openMenu = MenuAction, frag = Annotation, badge = Badge, confirm = ConfirmAction, secret = SecretReveal. */
declare global {
  interface Window {
    Swarog: {
      Panel: typeof Panel; DataTable: typeof DataTable; KpiTile: typeof KpiTile; PropertyList: typeof PropertyList;
      Timeline: typeof Timeline; Form: typeof Form; ConfirmAction: typeof ConfirmAction; SecretReveal: typeof SecretReveal;
      ServiceStatus: typeof ServiceStatus; TimeSeriesChart: typeof TimeSeriesChart; MenuAction: typeof MenuAction;
      Badge: typeof Badge; Annotation: typeof Annotation; status: typeof status; icon: typeof icon; ref: typeof ref;
      defineMenu: typeof defineMenu; rowMenuBtn: typeof rowMenuBtn; toast: typeof toast;
      openMenu: typeof MenuAction; frag: typeof Annotation; badge: typeof Badge; confirm: typeof ConfirmAction; secret: typeof SecretReveal;
      MODULE_COLORS: Record<string, string>;
    };
  }
}
