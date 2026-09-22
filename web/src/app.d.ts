declare global {
  namespace App {
    interface Error {
      message: string;
    }
  }

  var __TRELLIS_RUNTIME_CONFIG__:
    | {
      authUrl?: string;
    }
    | undefined;
}

export {};
