import { ReactNode } from "react";

export default function PageContainer({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={`max-w-[1472px] mx-auto w-full ${className}`.trim()}>
      {children}
    </div>
  );
}
