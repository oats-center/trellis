<script lang="ts">
  import Activity from "lucide-svelte/icons/activity";
  import ArrowRight from "lucide-svelte/icons/arrow-right";
  import Bell from "lucide-svelte/icons/bell";
  import Box from "lucide-svelte/icons/box";
  import ChevronDown from "lucide-svelte/icons/chevron-down";
  import Clipboard from "lucide-svelte/icons/clipboard";
  import Cpu from "lucide-svelte/icons/cpu";
  import Database from "lucide-svelte/icons/database";
  import Ellipsis from "lucide-svelte/icons/ellipsis";
  import Globe from "lucide-svelte/icons/globe";
  import Key from "lucide-svelte/icons/key";
  import LayoutGrid from "lucide-svelte/icons/layout-grid";
  import Maximize2 from "lucide-svelte/icons/maximize-2";
  import Menu from "lucide-svelte/icons/menu";
  import Minus from "lucide-svelte/icons/minus";
  import Moon from "lucide-svelte/icons/moon";
  import Pencil from "lucide-svelte/icons/pencil";
  import Phone from "lucide-svelte/icons/phone";
  import Plus from "lucide-svelte/icons/plus";
  import RefreshCw from "lucide-svelte/icons/refresh-cw";
  import Search from "lucide-svelte/icons/search";
  import Server from "lucide-svelte/icons/server";
  import Settings from "lucide-svelte/icons/settings";
  import Shield from "lucide-svelte/icons/shield";
  import Sun from "lucide-svelte/icons/sun";
  import TriangleAlert from "lucide-svelte/icons/triangle-alert";
  import Users from "lucide-svelte/icons/users";
  import X from "lucide-svelte/icons/x";

  type IconName =
    | "activity"
    | "alert"
    | "arrowRight"
    | "bell"
    | "box"
    | "chevronDown"
    | "clipboard"
    | "close"
    | "cpu"
    | "database"
    | "expand"
    | "grid"
    | "key"
    | "menu"
    | "moon"
    | "more"
    | "phone"
    | "plus"
    | "refresh"
    | "search"
    | "server"
    | "settings"
    | "shield"
    | "sun"
    | "users"
    | "globe"
    | "minus"
    | "pencil";

  const components = {
    activity: Activity,
    alert: TriangleAlert,
    arrowRight: ArrowRight,
    bell: Bell,
    box: Box,
    chevronDown: ChevronDown,
    clipboard: Clipboard,
    close: X,
    cpu: Cpu,
    database: Database,
    expand: Maximize2,
    grid: LayoutGrid,
    key: Key,
    menu: Menu,
    minus: Minus,
    moon: Moon,
    more: Ellipsis,
    pencil: Pencil,
    phone: Phone,
    plus: Plus,
    refresh: RefreshCw,
    search: Search,
    server: Server,
    settings: Settings,
    shield: Shield,
    sun: Sun,
    users: Users,
    globe: Globe,
  } as const;

  type Props = {
    name: IconName | string;
    size?: number;
    class?: string;
  };

  let { name, size = 18, class: className = "" }: Props = $props();
  let IconComponent = $derived(components[name as IconName] ?? Box);
</script>

<IconComponent {size} class={className} />
