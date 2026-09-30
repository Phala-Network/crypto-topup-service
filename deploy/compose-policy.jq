# The policy of a rendered, merged compose (docs/design/deploy-config.md §1.6), in one place for
# render.sh, validate-compose.sh, preflight.sh, and verify-attestation.sh. Input: the compose as
# `docker compose config --format json` prints it. `violations($variant; $project)` is the list of
# broken rules, empty when the compose passes. $variant is `service`, `restore-check` (the topup
# CVM, deploy/RESTORE.md), or `product` (the reference product); $project is `dstack` on a CVM.

def env_map:
    if type == "array" then map(capture("^(?<key>[^=]+)=(?<value>.*)$")) | from_entries
    elif . == null then {}
    else . end;

def check(ok; message): if ok then empty else message end;

def published: [.services | to_entries[] | select((.value.ports // []) | length > 0)
    | {service: .key, ports: .value.ports}];

# Only `$service` publishes, exactly `$port` → `$target` over TCP.
def only_published($service; $target; $port):
    published == [{service: $service, ports: [{mode: "ingress", target: $target,
        published: $port, protocol: "tcp"}]}];

def host_name: test("^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$");

# The login of a `postgres://USER@postgres:5432/topup` URL.
def database_user: capture("^postgres://(?<user>[a-z_]+)@postgres:5432/topup$").user // "";

def volume_sources($service): [.services[$service].volumes[]? | .source];

# The content of the config a service mounts at `$target`.
def mounted_config($service; $target):
    . as $root
    | [.services[$service].configs[]? | select(.target == $target) | .source]
    | if length == 1 then $root.configs[.[0]].content // "" else "" end;

# The top-level `public_origin` of a topup.yaml.
def config_origin:
    [split("\n")[] | capture("^public_origin:[ \\t]*[\"']?(?<origin>[^\"' \\t#]+)")] | first.origin // "";

# May the sealed secret `$name` fill the environment key `$key` of `$service`?
def secret_allowed($variant; $service; $key; $name):
    if $variant == "product" then
        $name == "PRODUCT_API_KEY" and $service == "product" and $key == $name
    else
        ($name == "SENTRY_DSN" and $service == "topup" and $key == $name)
        or (($name | test("^TOPUP_RPC_[A-Z0-9_]+_KEY$")) and $key == $name
            and ($service == "topup" or $service == "restore-check"))
        or (if $variant == "service" then
                ($name == "AWS_ACCESS_KEY_ID" or $name == "AWS_SECRET_ACCESS_KEY")
                and ($service == "postgres" or $service == "backup") and $key == $name
            else
                ($key == "AWS_ACCESS_KEY_ID" or $key == "AWS_SECRET_ACCESS_KEY")
                and $service == "postgres" and $name == "RESTORE_\($key)"
            end)
    end;

# Every string of the compose that Compose would interpolate (a `$` left once `$$` is removed) must
# be exactly one allowed `${NAME:-}` environment value: a sealed value can fill nothing else.
def secret_violations($variant):
    . as $root
    | [paths(type == "string") as $path
        | ($root | getpath($path)) as $value
        | select($value | gsub("\\$\\$"; "") | contains("$"))
        | ($value | capture("^\\$\\{(?<name>[A-Z_][A-Z0-9_]*):-\\}$").name // null) as $name
        | if ($path | length) == 4 and $path[0] == "services" and $path[2] == "environment"
                and $name != null and secret_allowed($variant; $path[1]; $path[3]; $name)
            then empty
            else "a sealed value may not fill \($path | map(tostring) | join("."))"
          end];

def common_violations($variant; $project):
    [ check(([.services[].image] | all(test("^[^@]+@sha256:[0-9a-f]{64}$") and (test("@sha256:0{64}$") | not)));
            "every image must be a nonzero repository@sha256 digest"),
      check(.name == $project; "the project must be \($project)"),
      check([.volumes // {} | to_entries[] | .value.name == "\($project)_\(.key)" and (.value.external | not)]
              | all; "every volume must be the project's own, named \($project)_<volume>"),
      check([.services[] | .build, .env_file, .extends, .profiles | select(. != null)] == [];
            "no service may build, read an env_file, extend, or carry a profile"),
      check([.configs // {} | to_entries[] | (.value.file == null) and (.value.content | type == "string")
              and (.key | test("^[a-z0-9_]+_[0-9a-f]{12}$"))] | all;
            "every config must be inline content named after its digest"),
      check([.services[] | .environment | env_map | to_entries[]
              | select((.key | test("PASSWORD$")) or ((.value // "") | test("postgres(ql)?://[^/@]*:[^/@]*@")))]
              == []; "no service environment may carry a password"),
      check([.services | to_entries[] | .key as $service | .value.volumes[]?
              | select(.type == "bind") | select(.source != "/var/run/dstack.sock")] == [];
            "only the dstack socket may be bind-mounted")
    ] + secret_violations($variant);

def topup_violations:
    (.services.topup.environment | env_map) as $topup
    | [ check(($topup.DATABASE_URL | database_user) == "topup_service"
                and $topup.PGPASSFILE == "/run/db-app/topup_service.pgpass";
              "topup must log in as topup_service with the application pgpass"),
        check((volume_sources("topup") | any(. == "db_owner" or . == "walg_key")) | not;
              "topup must mount neither db_owner nor walg_key"),
        check((.services.migrate.environment | env_map | .DATABASE_URL | database_user) == "postgres";
              "migrate must log in as the database owner"),
        check((mounted_config("topup"; "/etc/topup/topup.yaml") | config_origin | startswith("http"));
              "topup must mount its topup.yaml at /etc/topup/topup.yaml")
      ];

def service_violations:
    (.services["dstack-ingress"].environment | env_map) as $ingress
    | (mounted_config("topup"; "/etc/topup/topup.yaml") | config_origin) as $origin
    | (.services.smokescreen.command // []) as $smokescreen
    | [ check((.services | keys) == ["backup", "dstack-ingress", "heartbeat", "keys", "migrate",
                "postgres", "smokescreen", "topup"];
              "the service runs exactly keys, postgres, migrate, topup, dstack-ingress, smokescreen, heartbeat, and backup"),
        check(only_published("dstack-ingress"; 443; "443");
              "only dstack-ingress may publish a port, 443"),
        check($ingress.CHALLENGE_TYPE == "tls-alpn-01" and $ingress.TARGET_ENDPOINT == "topup:8080"
                and ($ingress.GATEWAY_DOMAIN // "" | host_name) and ($ingress.DOMAIN // "" | host_name)
                and $origin == "https://\($ingress.DOMAIN)";
              "dstack-ingress must serve the host of topup's public_origin with tls-alpn-01, forwarding to topup:8080, through a gateway host"),
        check(.services.topup.command == ["topup", "run", "--config", "/etc/topup/topup.yaml",
                "--webhook-proxy", "http://smokescreen:4750"];
              "topup must run the service with its webhooks through smokescreen"),
        check(.services.smokescreen.image == .services.topup.image and $smokescreen[0] == "smokescreen"
                and ($smokescreen | index("--listen-port=4750")) != null
                and ([$smokescreen[] | select(test("^--(allow|unsafe|upstream|egress-acl)"))] == [])
                and ((.services.smokescreen.ports // []) == []);
              "smokescreen must run unrelaxed from the service image, publishing nothing"),
        check((volume_sources("heartbeat") | any(. == "db_owner" or . == "walg_key")) | not;
              "heartbeat must mount neither db_owner nor walg_key"),
        check((volume_sources("dstack-ingress") | any(. == "db_owner" or . == "db_app" or . == "walg_key")) | not;
              "dstack-ingress must mount no credentials"),
        check([.services.postgres, .services.backup | .environment | env_map | .TOPUP_RESTORE_FROM_BACKUP // "off"]
                == ["off", "off"]; "the service must archive: TOPUP_RESTORE_FROM_BACKUP must not be on"),
        check([.services | to_entries[] | select(.value.volumes[]?.source == "/var/run/dstack.sock") | .key]
                | unique == ["dstack-ingress", "keys", "topup"];
              "only keys, topup, and dstack-ingress may mount the dstack socket")
      ] + topup_violations;

def restore_check_violations:
    (.services.postgres.environment | env_map) as $postgres
    | (.services.topup.command // []) as $command
    | (.services["restore-check"].environment | env_map) as $check
    | [ check((.services | keys) == ["keys", "migrate", "postgres", "restore-check", "topup"];
              "restore-check runs exactly keys, postgres, migrate, topup, and restore-check"),
        check(only_published("topup"; 8080; "8081"); "only topup may publish a port, 8081"),
        check($command[0:8] == ["topup", "run", "--config", "/etc/topup/topup.yaml", "--read-only",
                "--restore-report", "/run/topup-observability/restore-check.json", "--public-origin"]
                and ($command | length) == 9 and ($command[8] | test("^https://[^/]+$"));
              "topup must serve read-only under the restore instance's own https origin"),
        check($postgres.TOPUP_RESTORE_FROM_BACKUP == "on" and .services.postgres.command == null
                and .services.postgres.entrypoint == null;
              "postgres must restore with archiving off (TOPUP_RESTORE_FROM_BACKUP=on, the image's own entrypoint and command)"),
        check($postgres.AWS_ACCESS_KEY_ID == "${RESTORE_AWS_ACCESS_KEY_ID:-}"
                and $postgres.AWS_SECRET_ACCESS_KEY == "${RESTORE_AWS_SECRET_ACCESS_KEY:-}";
              "postgres must read the restore instance's own storage credentials"),
        check(.services["restore-check"].entrypoint[0:4] == ["topup", "restore-check", "--config", "/etc/topup/topup.yaml"]
                and ($check.DATABASE_URL | database_user) == "postgres"
                and (volume_sources("restore-check") | index("/var/run/dstack.sock")) != null;
              "restore-check must check the restored database as its owner, with the dstack socket"),
        check([.services | to_entries[] | select(.value.volumes[]?.source == "/var/run/dstack.sock") | .key]
                | unique == ["keys", "restore-check", "topup"];
              "only keys, topup, and restore-check may mount the dstack socket")
      ] + topup_violations;

def product_violations:
    (.services["dstack-ingress"].environment | env_map) as $ingress
    | (mounted_config("product"; "/etc/product/config.json") | fromjson? // {}) as $config
    | [ check((.services | keys) == ["dstack-ingress", "product"];
              "the product CVM runs exactly product and dstack-ingress"),
        check(only_published("dstack-ingress"; 443; "443"); "only dstack-ingress may publish a port, 443"),
        check($ingress.CHALLENGE_TYPE == "tls-alpn-01" and $ingress.TARGET_ENDPOINT == "product:8089"
                and ($ingress.GATEWAY_DOMAIN // "" | host_name) and ($ingress.DOMAIN // "" | host_name)
                and $config.public_url == "https://\($ingress.DOMAIN)";
              "dstack-ingress must serve the host of the product's public_url with tls-alpn-01, forwarding to product:8089"),
        check([.services | to_entries[] | select(.value.volumes[]?.source == "/var/run/dstack.sock") | .key]
                == ["dstack-ingress"]; "only dstack-ingress may mount the dstack socket")
      ];

def violations($variant; $project):
    common_violations($variant; $project)
    + if $variant == "service" then service_violations
      elif $variant == "restore-check" then restore_check_violations
      elif $variant == "product" then product_violations
      else ["unknown variant \($variant)"] end;
