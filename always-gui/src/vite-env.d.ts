/// <reference types="vite/client" />

declare module "*.tsx" {
  import React = require("react");
  export const Component: React.FC;
}

interface Window {
  __TAURI__?: {
    core: {
      listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void>;
      invoke<T>(command: string, args?: any): Promise<T>;
      send<T>(channel: string, args: { data: T }): void;
      getAll: () => any[];
    };
    window: {
      AppWindow: any;
      Window: new (label: string) => {
        loadUrl(url: string): Promise<void>;
        setVisible(visible: boolean): Promise<void>;
        setFocus(): Promise<void>;
      };
      WindowOptions: any;
    };
  };
}