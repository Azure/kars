import { redirect } from "next/navigation";

// The product opens on the Workspace (the employee surface). Operators switch
// to the Console from the header. Entitlement-based default routing lands with
// the auth slice.
export default function RootPage() {
  redirect("/workspace");
}
