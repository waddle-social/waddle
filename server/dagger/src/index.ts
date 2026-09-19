import { dag, Workspace, Directory, Container, object, func } from "@dagger.io/dagger"

@object()
export class WaddleServer {
  /** Image the build runs on. */
  @func()
  baseImageAddress: string

  source: Directory

  constructor(
    ws: Workspace,
    baseImageAddress = "alpine:3.21",
  ) {
    this.source = ws.directory("/", {
      exclude: ["**/node_modules", "**/.git", "**/dist", "**/.dagger"],
    })
    this.baseImageAddress = baseImageAddress
  }

  /** A container with the source mounted, ready to build on. */
  @func()
  container(): Container {
    return dag
      .container()
      .from(this.baseImageAddress)
      .withDirectory("/src", this.source)
      .withWorkdir("/src")
  }
}
