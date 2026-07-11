import { MARKETING } from "../content"

export const metadata = { title: MARKETING.openSource.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.openSource.html }} />
}
