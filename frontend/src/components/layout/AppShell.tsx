import type { ReactNode } from 'react';
import { Sidebar } from './Sidebar';
import { Header } from './Header';
import { StatusBar } from './StatusBar';
import { useWebSocket } from '../../hooks/useWebSocket';

interface Props {
  children: ReactNode;
}

export function AppShell({ children }: Props) {
  useWebSocket();

  return (
    <div className="flex h-screen overflow-hidden bg-app-bg">
      <Sidebar />
      <div className="flex flex-col flex-1 min-w-0">
        <Header />
        <main className="flex-1 overflow-auto px-5 py-5">
          <div className="mx-auto max-w-[1600px] w-full">{children}</div>
        </main>
        <StatusBar />
      </div>
    </div>
  );
}
