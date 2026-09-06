# hwahap v4

기본적으로 `PLAN → BUILD → ADJUST → SHIPPING`을 진행하며 PLAN과 BUILD를 따로 사용할 수 있는
Codex 스킬과 local STDIO MCP 서버다. BUILD는 구현·검증·draft PR 검토를 포함하고,
ADJUST에서 계약 변경은 PLAN으로, 계약 내 구현 수정은 BUILD로 돌아간다. Rust 실행기는 계획·검증·복구를 담당하고,
호스트 Codex가 기본 하위 에이전트를 실행한다. 진행 상태와 실행 요청은 `.hwahap/`에 저장한다.

지원하는 실행 계약은 `hwahap/v4` 하나다. 다른 버전의 계약·리뷰·실행 기록은 자동 변환하거나
승인을 이어받지 않는다. 이전 설치와 hook을 제거한 뒤 현재 스킬과 release를 함께 설치한다.
보존할 대화·결정·검증 기록은 활성 `.hwahap` 저장소 밖의 이력으로 둔다.

## 1. 한눈에 보기

| 단계 | 실행 주체 | 하는 일 | 사람이 하는 일 |
|---|---|---|---|
| PLAN | Economy(사실) + Deep(결정) | 선행 조건이 해결된 질문을 제시하고, 답변 뒤 새 쟁점을 찾아 다음 라운드를 구성 | 질문 UI에서 선택하거나 원문 답변 |
| PLAN FREEZE | Rust validator + Deep Auditor + Critic | ID 연결·의존관계·필수 필드 검사, 작성자와 독립된 계약 검토 | `CONFIRM PLAN <challenge>` 정확히 입력 |
| PLAN READY | Rust 실행기 | `plan_only:true`인 계획을 확정하고 대기 | 원할 때 확정 계획의 BUILD를 명시적으로 요청 |
| CODING | Economy(첫 구현) + Deep(재작업) + Critic(리뷰) | unit을 순서대로 구현·검증·리뷰하고 통과한 변경을 commit | 없음. 승인 범위가 충돌하면 중단 |
| DRAFT PR / PR REVIEW | Astra Critic + 별도 Astra Auditor | full suite 후 draft 게시, 공격 보고서·방어 판정, 확인된 결함 수정 후 새 head 재검토 | 결과 확인 |
| ADJUST / SHIP | — | 계약 변경은 PLAN 재확인, 계약 내 수정은 BUILD 재검증, 완료된 draft는 ready로 | 변경 의도 전달 또는 `SHIP <challenge>` 정확히 입력 |

`request`와 `plan_only:true`로 시작하면 `CONFIRM PLAN` 이후 `plan_ready`에서 끝난다. 구현은 이후 사용자의
명시적 BUILD 요청과 `build_confirmed:<전체 plan_digest>`로 시작한다. `plan_only` 기본값 `false`인 일반
구현 요청은 계획 확인 뒤 구현까지 이어진다. 두 경로의 호출 예시는 [USAGE](USAGE.md)를 따른다.

일반 PLAN 경로의 확인 문장은 `CONFIRM PLAN`과 `SHIP`이다. 둘 다 내용에 결속된 digest challenge라서, 계획이
바뀌면 이전 challenge는 더 이상 맞지 않는다. 호스트 모델은 이 문장을 생성·보완·추론할 수 없다.

사용자가 기획 생략을 명시하면 `hwahap_step.build`에 원문 권한, 목표, 기준·작업 브랜치,
unit별 acceptance·경로·테스트와 full suite를 전달한다. 실행기는 이 BUILD 계약을 검증·고정하고
바로 구현한다. 계획 답변·검토·`CONFIRM PLAN`은 만들지 않는다. `SHIP` 권한은 별도다.
같은 계약의 구현 수정은 사용자 원문·계약 digest·unit ID를 가진 `adjust_build`로 요청한다.
acceptance·테스트·허용 경로를 바꾸는 요청은 `user_input`으로 PLAN을 다시 연다.

## 2. 설치

hwahap은 스킬 하나와 MCP 서버 하나로 이루어진다. 둘을 따로 설치한다.

```sh
# 현재 소스를 검증한 뒤 release를 빌드한다.
cargo test --manifest-path runtime/Cargo.toml --all-targets
cargo build --release --manifest-path runtime/Cargo.toml
bin/hwahap --version

# 기존 설치를 제거한 빈 경로에 현재 스킬과 release만 설치한다.
hwahap_install="${CODEX_HOME:-$HOME/.codex}/skills/hwahap"
test ! -e "$hwahap_install"
mkdir -p "$hwahap_install/runtime/target/release"
rsync -a --exclude target --exclude .git --exclude .hwahap ./ "$hwahap_install/"
cp runtime/target/release/hwahap "$hwahap_install/runtime/target/release/"
codex mcp add hwahap -- "$hwahap_install/bin/hwahap"
```

`bin/hwahap`은 같은 설치의 `runtime/target/release/hwahap`만 실행하고 `--version`이
`version.txt`에 기록된 버전과 일치하는지 확인한다. 환경변수·debug·PATH의 다른 바이너리를 탐색하지 않는다.
진단은 stderr로, MCP 응답은 stdout으로 보낸다. 등록 후 Codex에서 연결을 새로 열어
`initialize`의 서버 버전과 제공 도구를 확인한다. 등록 명령은 [공식 MCP 문서](https://developers.openai.com/codex/mcp)를 따른다.

필요한 것: Rust 1.90 이상, POSIX 환경, `git`, 인증된 `gh`, 기본 하위 에이전트의 생성·follow-up·대기·중단 도구를
제공하는 Codex 호스트다. `.hwahap/`은 대상 저장소의 `.gitignore`에 있어야 한다.
런처와 스킬을 설치한 뒤 실제 호스트에서 MCP 연결과 하위 에이전트 도구의 제공 여부를 확인한다.

이 저장소 루트가 스킬 디렉터리다. 스킬 복사와 MCP 등록을 분리한다.
플랫폼의 보장과 이 구현에서 확인한 범위는 [PLATFORM.md](PLATFORM.md)에 기록한다.
일반 개발 사이클, 큰 기능의 단계별 진행, 승인·복구와 완료 보고는
[운영 절차](OPERATIONS.md)를 따른다.

## 3. 구조

```
hwahap/
├── SKILL.md                       호스트를 MCP 실행 절차로 연결
├── README.md                      이 문서
├── PLATFORM.md                    플랫폼 근거, 검증 범위와 한계
├── bin/hwahap                     바이너리를 찾아 exec 하는 POSIX sh 런처
├── tests/gates.sh                 정적 단순성 게이트
└── runtime/                       Rust 크레이트
    ├── src/                       모듈당 책임 하나
    └── tests/
        ├── common/mod.rs          실제 저장소 + gh stub + 스크립트 에이전트
        ├── cycle.rs               스크립트로 도는 전체 사이클
        ├── surface.rs             크레이트 밖에서 본 MCP 표면
        └── native_surface.rs      native 요청·등록·완료·복구 프로토콜
```

런타임 모듈은 각각 한 가지만 안다.

| 모듈 | 책임 |
|---|---|
| `canonical` | canonical JSON과 digest. challenge가 나오는 유일한 곳 |
| `plan` | `hwahap/v4` 계약 타입. 답변 신선도 규칙 |
| `answer` | 원문 사용자 메시지와 정확한 확인 문장의 문법 |
| `dialogue` | 계획에 결속된 질문 배치와 구조화 응답 검사 |
| `frontier` | 지금 물을 수 있는 질문 |
| `validate` | freeze 게이트와 unit 위상 정렬 |
| `render` | 결정적 `plan.md` |
| `state` | `run.json` 원자적 스냅샷 + `events.jsonl` hash chain |
| `profile` | 고정 profile 3개. `none`/`low`/`max`는 타입상 존재하지 않는다 |
| `native` | 요청 저장, 호스트 전달, agent 등록·완료·중단 확인, 실행 잠금 |
| `session` | 실행 결과와 증거 출처를 구분하는 타입 |
| `cost` | 요청·완료·미보고 사용량과 모델별 보고 토큰 집계 |
| `agentresult`·`proposal` | agent가 낼 수 있는 strict JSON 계약 |
| `prompts` | 역할별 프롬프트. 같은 입력이면 같은 바이트 |
| `git`·`forge` | 실제 관찰면. 성공은 여기서만 판정된다 |
| `engine` | 상태 기계 |
| `mcp` | tool 3개와 `instructions` |

## 4. 설계 결정

**MCP tool은 3개뿐이다.** `plan`, `cycle`, `retry`, `spawn_worker` 같은 tool은 전부 스케줄링·승인
판단을 호출한 모델에게 되돌려준다. 그 판단은 Rust 상태 기계가 한다.

**cross-tool protocol의 단일 출처는 MCP `instructions`다.** `SKILL.md`도, 어떤 참조 문서도 같은
규칙을 반복하지 않는다.

**추천은 사용자가 선택한다.** 질문 배치에는 대안의 실제 내용과 추천 근거를 표시한다. 구조화 응답은
현재 계획과 질문 ID·정확한 label에 결속된다. 빈 응답·취소·시간 경과는 추천 선택이 되지 않는다.
다른 원문은 `Clarify`로 남겨 해석을 다시 묻는다. 기존 답변의 질문·추천 신선도 검사도 유지한다.

**질문 UI는 호스트 기능이다.** 호출 가능한 Codex 질문 도구와 그 제한을 확인해 배치를 전달한다.
대안이 많으면 전체 label을 보여주는 자유입력 UI를 사용하고, UI가 없으면 전체 텍스트와 원문 응답을
보존한다. `AskUserQuestion` 도구나 Codex 모드 전환을 만들어내지 않는다. [전달 규칙](USAGE.md#질문-ui와-원문-응답)을 따른다.

**기획 완료는 기록한 계약의 완료다.** 새로운 대화형 계획은 시작 시 기록한 source commit에 결속한다.
이미 저장된 계획은 기존 digest 계산을 유지한다. 질문·리뷰·응답 결속은 호스트가 전달한 기록에 기반하며
사용자 신원을 암호학적으로 인증하거나 모든 중요 결정의 누락·오해가 없음을 증명하지 않는다.

**LLM의 주장은 증거가 아니다.** worker의 JSON은 control metadata일 뿐이고, 테스트 통과는 호스트가
명령을 실행해 exit status로, 변경 범위는 git diff로 판정한다. 리뷰 세션이 working tree를 건드렸으면
그 verdict는 폐기된다.

**호스트는 요청된 모델과 effort를 그대로 전달한다.** 새 하위 에이전트만 `fork_turns=none`으로
생성하고, 유지한 작업자에게는 현재 지시를 follow-up으로 전달한다. 도구나 모델이 없으면 한계를 보고하고
중단한다. 실행 요청 기록은 실제 적용된 모델을 독립 검증한 증거가 아니다. 상세 호출 절차의 단일 출처는 MCP `instructions`다.

**작업 경로와 read-only는 지침이다.** 현재 호스트의 spawn 호출에는 별도 `cwd`나 `sandbox` 필드가
없다. 절대 경로와 접근 범위를 작업 지시에 담고, 검토 전후 Git 상태를 검사한다. 이 검사는 OS 수준의
파일 접근 격리나 모든 외부 부작용 방지를 증명하지 않는다.

**SQLite도 daemon도 없다.** 저장소당 active run 하나뿐이므로 `run.json`(원자적 교체)과
`events.jsonl`(hash chain)이면 충분하다. 스냅샷이 journal보다 앞서 있으면 fail-closed다.

**진행 중인 에이전트를 확인한 뒤 복구한다.** 실행 요청은 전달 전에 저장하고, 호스트는 생성 직후
agent ID를 등록한다. 완료 기록은 결과 전달 전에 저장하며 같은 완료의 재전송은 중복 실행하지 않는다.
재시작·시간 초과로 남은 작업은 호스트가 에이전트와 명령의 종료를 확인해야 복구할 수 있다.

## 5. 모델·effort 정책

| Profile | 모델 | Effort | 담당 |
|---|---|---|---|
| Economy | `gpt-5.6-luna` | `medium` | Worker: 사실 조사, 첫 구현 |
| Critic | `gpt-6-astra` | `high` | Critic: plan·unit 리뷰, PR 공격 |
| Deep | `gpt-6-astra` | `high` | 부모: 추천·합성·재계획·재작업; 별도 Auditor: ColdConsumer·PR 방어 |

PLAN을 거쳐 BUILD를 시작하면 Luna가 첫 구현을 맡고, direct BUILD는 부모 Astra가 첫 구현도 맡는다.
실패하면 부모 Astra가 한 번 재작업하고, 다시 실패하면 근거와 함께 중단한다.
부모는 Astra이며 추천·plan 합성·PlanConflict replan·재작업을 직접 처리한다.
Worker·Critic·Auditor 세 자식은 같은 저장소와 같은 `host_session_id` 안에서 unit과 run을 넘어 유지한다.
완료마다 닫고 새로 만들지 않는다. ColdConsumer는 작성자와 독립된 계약 검토자이며,
재사용된 Auditor에 과거 검토 문맥이 남을 수 있으므로 매번 완전히 새 문맥이라는 보장은 없다.
Worker·Critic·Auditor 사이의 전환, 모델·effort 변경이나 사라진 작업자는 교체 spawn으로 우회하지 않는다.
direct BUILD에는 Worker가 필요 없고 Critic·Auditor 두 자식만 유지한다. 처음 필요한 자식 슬롯은
일반 경로 세 개, direct BUILD 두 개다. 다른 저장소·부모는 별도 pool이며 전역 quota 회복은 보장하지 않는다.

`.hwahap/config.toml`의 `[profiles.*]`에서 model과 effort를 함께 지정할 수 있지만 이미 유지 중인 pool과 다르면 실행을 거부한다.
부모가 처리하는 Deep 역할과 두 검토자는 `gpt-6-astra`를 요구하며 다른 모델 설정은 dispatch 전에 거부한다.
direct BUILD는 Economy 역할도 부모 Astra로 고정한다. `build-request.json`에 BUILD를 시작한 부모를 즉시 결속하고, `native-owner.json`으로 소유권을 유지한다.
`[limits]`의 기본값은 `native_max_calls=64`, `native_timeout_secs=180`이다. 요청 한도에는
재시도와 follow-up도 포함된다. soft 목표는 역할별 60/90/120초이며 native 요청의 hard 제한은 180초다.
이는 테스트·Git·GitHub 명령을 포함한 전체 작업의 시간 상한이 아니다.
시간 초과는 자동 종료 증거가 아니므로 중단 확인이 필요하다. 구체적 분류·관측 기록은 [PLATFORM](PLATFORM.md#2-저장과-중단-복구)을 따른다.
이 요청 예산은 호스트의 열린 thread 한도와 다르다. 생성 거절은 `native_paused`로 기록하고,
새 회복 근거가 있을 때 기존 run을 재개한다. [운영 절차](OPERATIONS.md#6-중단-상태별-대응)를 따른다.

총비용 개선은 불필요한 계획용 하위 에이전트 생성과 반복 실패를 줄이는 방향이다. 상태·보고서에는
요청·완료·중단·미완료·생성 실패·복구 수, requested model별 보고 토큰과 보고 비율을 남긴다. 호스트 처리와 하위
에이전트의 사용량 보고 비율은 구분한다. 명시적으로 등록한 부모·자식 세션의 누적 카운터 차이를 `.hwahap/usage.json`에 저장한다.
등록 전 작업과 수집 실패는 누락으로 표시하며, 세션 합계와 dispatch 합계는 중복되므로 더하지 않는다.
선택한 단가표로 추정 비용을 계산할 수 있다. 실제 청구액과 모델별 절감 효과는 별도 검증이 필요하다.
명령, 단가표와 Terra Economy 설정은 [사용량 계측](USAGE.md)을 따른다.

PR 공격·방어 결과는 PR URL·head SHA·계약 digest에 결속한다. 방어자는 공격 항목마다
`confirmed/refuted/unresolved`와 근거를 제출한다. 미해결은 중단하고, 확인된 결함은 부모가 수정해
모든 구현 unit의 고정 테스트와 full suite·commit·같은 PR push 이후 두 팀이 다시 검토한다.
테스트 실패 시 명령·출력·patch를 보존한 뒤 해당 시도를 초기화하고 남은 예산으로 재시도한다.
리뷰 요청은 commit 범위와 파일 목록을 전달하며, 각 검토자가 로컬에서 전체 diff를 읽는다.
PR 보고서는 검토·수정 후와 SHIP 직전에 현재 검토·사용량 근거로 갱신한다. 저장된 공격 보고서는 방어 단절 후 재사용한다.
`hwahap_step(recheck_pr=true)`는 이 run의 기존 draft를 다시 검증하며 수정 예산을 초기화하지 않는다.

## 6. 테스트 규칙

자동 테스트는 실제 임시 Git 저장소와 스크립트 실행 결과로 재작업·범위 이탈·계획 충돌·복구·ship
검사를 재현한다. native 인터페이스 테스트는 요청·등록·완료·중단 확인과 사용량 누락을 검사한다.
통제된 호스트의 [pool 테스트](runtime/tests/native_pool.rs)는 300개 작업을 생성 3회·재사용 297회로 처리했다.
질문 배치 테스트는 대안 보존·응답 결속·원문 보존과 잘못된 입력 거부를 검사한다.
이 결과는 실제 Codex 질문 UI의 표시·전달이나 일반 PLAN부터 PR까지의 전체 모델 실행, 모델별 비용 비교를
대신하지 않는다. 문서 변경 자체가 설치된 스킬과 MCP 바이너리를 갱신하지도 않는다.

```sh
cargo test --manifest-path runtime/Cargo.toml --all-targets
cargo clippy --manifest-path runtime/Cargo.toml --all-targets -- -D warnings
tests/gates.sh
```

`gates.sh`는 tool 수, 스킬 크기, 기본 모델·effort 및 금지된 실행 경로 같은 정적 계약을 검사한다.

## 7. 하지 않는 것

merge와 auto-merge, force-push, history rewrite, 상시 daemon, HTTP MCP transport, 병렬 worker pool,
worker용 내부 MCP server, ACP proxy/conductor/protocol v2, cross-project memory, v2 아티팩트 변환.
