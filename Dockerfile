FROM mcr.microsoft.com/dotnet/sdk:8.0

# mono provides the .NET Framework tooling bits the net472 build reaches for.
RUN apt-get update && apt-get install -y --no-install-recommends \
        mono-complete \
    && rm -rf /var/lib/apt/lists/*

# HOME/DOTNET_CLI_HOME must be writable: the container runs as an arbitrary
# non-root uid, which otherwise has no home directory to initialise into.
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1 \
    DOTNET_NOLOGO=1 \
    NUGET_PACKAGES=/nuget \
    HOME=/tmp \
    DOTNET_CLI_HOME=/tmp

# The MonoGame content builder (mgcb) still targets netcoreapp3.1; roll it forward
# onto the .NET 8 runtime this image ships, for when content building is enabled.
ENV DOTNET_ROLL_FORWARD=LatestMajor

RUN mkdir -p /nuget && chmod -R a+rwX /nuget

WORKDIR /workspace

# Content building is skipped by default -- see Directory.Build.targets. The .spritefont
# assets request "Consolas", a Microsoft font that cannot be shipped here, and MonoGame
# resolves it by scanning for a font file with that exact family name (not via fontconfig),
# so the content pipeline cannot complete on Linux. Code changes don't affect content:
# copy the built assembly into an existing install, which already has its content built.
CMD ["dotnet", "build", "Blish HUD.sln", "-c", "Release", "-p:SkipContentBuild=true", "-p:EnableMGCBItems=false"]
