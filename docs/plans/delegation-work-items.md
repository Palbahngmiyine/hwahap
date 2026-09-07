# 구현 작업 명세

기준 커밋: `6a3689d`. [설계 요약](delegation.md)의 7개 작업을 구현 파일·계약·검증으로 연결한다.
아래 타입·함수·테스트 이름 중 **신규** 표시는 구현할 대상으로, 나머지는 현재 소스의 연결 지점이다.
파일 경로는 `plugins/hwahap/runtime/` 기준이다. 작업 순서는 **U1 → U3 → U2 → U4 → U5 → U6 → U7**이다.

| 작업 | 인수할 결과 | 전용 테스트 | 완료 판정 |
| --- | --- | --- | --- |
| U1 | 완료 조건 누락을 찾는 공통 validator | `unit_verification` | 모든 입력 경로가 같은 오류 반환 |
| U3 | 실행·복구·SHIP이 소비하는 검증 기록 | `verification_evidence` | 후보 코드가 달라지면 성공 증거 무효화 |
| U2 | 검토 지적을 해결 경로로 보내는 상태 전이 | `decomposition_review` | 구조 수정 후 독립 재검토까지 연결 |
| U4 | 구현과 재검증을 구분하는 실행 경로 | `unit_revalidation` | 자격 있는 작업은 테스트만으로 완료 |
| U5 | 실행별 모델·effort 결속과 가용성 검증 | `capability_catalog` | 새 카탈로그로 기존 배정 변화 0건 |
| U6 | 평가 결과가 실제 dispatch에 반영되는 정책 | `delegation_policy` | 판정표와 생성·재사용 결과 일치 |
| U7 | 통합 회귀와 사용 절차 | `delegation_flow` | T01–T17 및 전체 검사 통과 |

<details>
<summary>U1 · 완료 조건의 작업별 검증</summary>

**수정 지점:** `src/validate.rs::check_verification`, `tests/unit_verification.rs`(신규). 입력 경로 회귀는 `tests/direct_build.rs`, `tests/approved_plan.rs`, `tests/cycle.rs`에 추가한다.

1. 각 비-probe unit의 acceptance 집합에서 자체 테스트가 참조하는 acceptance 합집합을 뺀다.
2. 남은 항목마다 `uncovered_unit_acceptance`와 unit·acceptance ID를 반환한다. 기존 `untested_unit`, `test_outside_unit` 검사와 안정적인 오류 정렬을 함께 적용한다.
3. `freeze_blockers`, `build_blockers`, `approved_plan_blockers`를 통해 같은 검사를 소비한다. 거부된 입력은 기존 계획과 승인 기록을 보존한다.

**검증:** T01의 U1/A2 누락, T02의 합집합·공통 acceptance 정상 사례, T03의 빈 값·외부 참조·순환, T04의 입력 경로별 거부를 검사한다. direct BUILD는 현재 입력 형식으로 표현 가능한 반례를 사용한다.

**인계:** 다음 작업은 이 공통 검사를 통과한 unit·acceptance·test 연결을 사용한다.

</details>

<details>
<summary>U3 · 검증 기록과 중단 복구</summary>

**수정 지점:** `src/verification.rs`(신규), `src/lib.rs`, `src/state.rs`, `src/plan.rs`, `src/engine.rs::{verify_unit,finalize,run_command}`, `src/engine/pr_review.rs::require_completed_reviews`, `src/engine/pr_review/repair.rs::repair_pr`, `src/git.rs`, `tests/verification_evidence.rs`(신규).

**입력·출력:** 신규 `VerificationRequest`는 run·unit/test ID·kind·attempt·계약 digest·명령·cwd·코드 식별자를 받는다. 신규 `VerificationRecord`는 요청, 결과 상태, 종료 코드, 출력 경로·해시를 보존한다. 상태는 `started`, `passed`, `failed`, `interrupted`다.

| 시점 | 코드 식별자 | 성공 결과 소비 |
| --- | --- | --- |
| 커밋 전 unit 테스트 | base HEAD + `Git::fingerprint` + 예정 tree | 해당 구현 시도의 검증으로 사용 |
| 커밋 후 구현 완료 | 실제 commit + tree + 앞선 기록 ID | commit tree와 검증한 tree가 같을 때 완료 기록 생성 |
| 최종 검증·재검증 | 현재 HEAD + tree + fingerprint | 현재 계약과 코드가 모두 일치할 때 사용 |

- 커밋 전 검증은 허용 경로 확인 → 작업 소유 변경 stage → `git write-tree` → fingerprint 기록 → 테스트 순서다. 테스트·독립 검토 후 같은 tree인지 확인하고 실제 commit tree와 연결한다.
- 명령 전 의도 이벤트, 명령 후 출력 파일, 완료 이벤트, snapshot 순서로 저장한다. 완료 이벤트에 전체 기록과 출력 해시를 넣어 journal 재생으로 snapshot을 복구한다.
- 완료 이벤트 이전 중단은 `interrupted`로 처리하고 소유 프로세스 종료 확인 후 새 시도로 실행한다. 동일 ID·동일 내용의 완료는 멱등 처리하고, 같은 ID의 다른 내용은 충돌로 반환한다.
- 실행 전후 fingerprint로 HEAD·index·추적 파일·비무시 신규 파일을 비교한다. 테스트가 사용하는 ignored 입력은 선언된 입력 목록의 해시로 추가 결속한다. build 산출물과 `.hwahap` 기록은 별도로 관리한다.
- 입력 목록은 신규 `Plan.verification_inputs: Vec<String>`에 저장한다. 저장소 상대 파일·디렉터리 경로를 정렬해 내용·모드·존재 여부를 해시하고, 경로의 저장소 경계와 symlink 대상을 검사한다. 목록 변경은 계약 digest와 unit fingerprint를 갱신한다.
- 최종 검증은 `(unit_id,test_id)`마다 실행하고 마지막에 full_suite를 실행한다. 같은 명령을 가진 서로 다른 작업도 각 검증 의무를 기록한다. 변경된 HEAD에서는 최종 검증과 양측 PR 검토를 다시 수행한다.
- `hwahap/v5`는 plan·run·검증 기록에 함께 적용한다. v4 입력은 쓰기 전 거부한다. schema를 가정하는 기존 테스트와 정적 게이트도 같은 작업에서 갱신한다.

**검증:** T11–T14, T16–T17. 저장 단계마다 중단을 주입하고 결과 미확정·재실행·snapshot 재구성을 각각 확인한다. 커밋 전후 같은 tree, 테스트 자체 수정, 출력 유실도 검사한다.

**인계:** `record_verification`, `recover_verifications`, `require_current_verifications`(신규)를 U4와 최종 승인 경로가 공유한다.

</details>

<details>
<summary>U2 · 검토 지적을 실제 해결 경로로 연결</summary>

**수정 지점:** `src/plan.rs::PlanReview`, `src/agentresult.rs`, `src/proposal.rs`, `src/prompts.rs`, `src/engine.rs::prove`, `src/state.rs`, `src/render.rs`, `tests/decomposition_review.rs`(신규).

**계약:** 신규 `PlanningFinding`은 `id`, `parent_id`, `kind`, `targets`, `evidence`, `expected`, `status`를 갖는다. kind는 `choice/fact/structure/blocker`, status는 `open/resolved`다. 기존 unit·PR 검토 결과와 분리된 계획 검토 계약을 사용한다.

| 지적 | 다음 행동 | 완료 조건 |
| --- | --- | --- |
| fact | FactFinder로 필요한 근거 조사 | 현재 소스에 결속된 사실 확보 |
| choice | 사용자에게 한 문장 질문 | 실제 답변 기록 |
| structure | 부모의 구조 수정 | 공통 validator와 새 digest의 두 독립 검토 통과 |
| blocker | 필요한 근거·권한을 표시하고 대기 | 해당 blocker의 해소 증거 확보 |

- 혼합 지적은 하위 ID와 의존 관계로 분리한다. 새로운 사용자 선택이 선행되면 그 답변을 받은 뒤 구조를 수정한다.
- 승인 전 구조 수정은 같은 요구·선택 의미 아래 unit과 테스트 연결을 바꾼다. 의미 보존은 독립 검토가 확인한다. frozen acceptance·명령·경로·의존성·권한 변경은 기존 계약 변경 경로로 보낸다.
- 분해 근거와 해결 이력은 review digest에 포함하고 unit fingerprint와 분리한다. 원본 지적의 종료는 새 검토의 해결 증거에 결속한다.
- run 전체 구조 수정 예산 3회를 시도 전에 journal에 기록한다. 재분할·재시작 이후에도 누적한다. 수정 예산 종료는 미해결 지적과 마지막 검증 결과를 보존한다.

**검증:** T05–T08, T15. 구조 지적만 있는 fixture는 새 사용자 decision 생성 0회, 부모 수정 1회, 새 digest의 독립 검토 2회를 기대한다. 새 선택이 섞인 fixture는 질문 대기를 기대한다.

</details>

<details>
<summary>U4 · 재검증 자격과 직접 수정 의무</summary>

**수정 지점:** `src/engine.rs::{code,build_unit,verify_unit,invalidated_units}`, `src/engine/adjust_build.rs::adjust_build`, `src/engine/pr_review/repair.rs`, `src/state.rs`, `tests/unit_revalidation.rs`(신규).

**계약:** 신규 `ImplementationRecord`는 run·unit·unit fingerprint·commit·tree·검증 기록 ID를 저장한다. 신규 `RepairObligation`은 직접 대상 ID, 근거 유형·참조, 계약 digest를 저장한다. 의존성에 따른 무효화는 직접 의무와 구분한다.

- ADJUST의 명시된 unit ID와 확정 PR finding의 파일 소유 unit을 직접 대상으로 기록한다. 디렉터리 경로는 경계 단위로 비교하며, 겹친 소유 경로는 모두 포함한다.
- 대상이 불명확하면 Critic의 영향 검토로 연결을 보완한다. 연결이 확정될 때까지 해당 작업은 대기한다.
- 같은 run의 완료 기록·동일 fingerprint·직접 의무 부재가 모두 참이면 `revalidate_unit`(신규)로 분기한다. 이 경로는 고정 테스트와 현재 후보의 영향 검토를 실행한다.
- U4 단계에서는 재검증마다 Critic 영향 검토를 적용한다. U6에서 낮은 위험·독립 작업에 한해 고정 테스트로 충분한 경로를 추가한다. 이 순서로 후속 위험 타입에 대한 의존성을 해소한다.
- 재검증 성공은 accepted 상태와 현재 검증 기록을 갱신한다. 명령 실패·timeout·코드 변화는 실패 시도를 보존한다. 구현·재검증 예산은 각 unit의 run 내 누적 횟수로 관리한다.

**검증:** T09–T12, T15. 완료된 의존 작업 재검증에서 Implementer/Rework 호출 0회·commit 증가 0건을 확인한다. 직접 수정 대상과 변경된 계약은 구현 경로를 기대한다.

</details>

<details>
<summary>U5 · 모델 설정부터 native 배정까지 연결</summary>

**수정 지점:** `src/catalog.rs`(신규), `src/config.rs::Config::for_run`, `src/profile.rs`, `src/session.rs`, `src/native.rs`, `src/native/{host,pool,broker,reply}.rs`, `src/native/broker/dispatch.rs`, `src/mcp.rs`, `src/state.rs`, `tests/capability_catalog.rs`(신규), `tests/gates.sh`(저장소 루트).

- `CatalogSnapshot`과 `HostObservation`(신규)을 분리한다. snapshot은 run 생성 시 고정하고, 관찰은 dispatch 전 갱신한다. 각 모델 ID·effort 문자열은 카탈로그와 호스트 양쪽에서 정확히 일치해야 한다.
- effort는 검증된 문자열 타입으로 전달한다. 지원 depth와 선호 순서는 카탈로그가 제공한다. 기존 enum·parser·receipt·gate의 고정 effort 가정도 함께 갱신한다.
- 역할의 읽기/쓰기 권한과 작성자/검토자 lane은 유지한다. `Profile`은 역할 분류로 남기고 모델명 조건은 카탈로그 조회로 바꾼다. `SessionReceipt::verify`도 선택 기록과 모델·effort·역할의 결속을 검사한다.
- U5에서는 기존 역할 배정에 대해 카탈로그의 `role_requirements`를 사용한다. U6에서 작업별 요구를 더한다. 따라서 카탈로그는 이 작업부터 실제 dispatch 생성과 재사용 검증에 사용된다.
- 각 역할 요구는 `capabilities`와 `depth`를 갖는다. 기본값은 아래 표의 설계 정책이며, 모델의 실제 역량 측정값과 구분한다.

| 역할 | 최소 역량 | depth |
| --- | --- | --- |
| FactFinder | repository_analysis=1 | routine |
| Implementer | implementation=2 | focused |
| PlanCritic, UnitReviewer, FailureDiagnosis | repository_analysis=2, adversarial_review=2 | deep |
| ColdConsumer, FinalReview | adversarial_review=2, security_review=2 | deep |
| Recommender, PlanSynthesis, ConflictReplan, Rework | cross_module_reasoning=2, implementation=2 | deep |

- 호스트 관찰은 부모 ID·관찰 시각·출처·모델/effort·도구·slot을 갖는다. 현재보다 미래인 시각, 300초 초과, 이전 관찰로의 역행은 거부한다. 관찰 갱신은 한 native action에 동반 가능한 메타데이터다.
- 신선도 검사는 새 배정 직전에 적용한다. 기존 dispatch의 completion·stop은 저장된 identity로 검증·기록한 뒤 다음 배정에 필요한 관찰을 요청한다.
- pool 키는 run ID와 부모 ID를 함께 사용한다. stop 완료는 작업 종료를 뜻하고 slot 해제 여부는 별도 호스트 관찰로 확인한다. slot이 부족하면 새 부모 작업에서 새 run을 시작할 수 있도록 안내한다.
- `abandon`은 run ID·원문 종료 지시·현재 계약 digest에 결속한다. `abandon_requested → stop 확인 → archived`를 journal에 기록하고, 각 단계 재시도는 같은 종료 결과로 수렴한다. worktree와 사용자 변경은 보존한다.
- archive는 run ID로 고정한 대상과 파일별 해시 manifest를 먼저 기록한다. 파일 이동 중 중단되면 원본/대상의 해시를 검사해 남은 이동만 수행하고, 전체 기록 보존을 확인한 뒤 새 run을 허용한다.
- 명시한 catalog 경로의 파일 오류는 설정 오류로 반환한다. 기본 파일이 없으면 번들 선언을 사용한다. legacy profiles는 변환할 설정과 적용 시점을 안내한다.

**검증:** 임의 모델 `model-a → model-b`, effort `quick/deep`, 만료 관찰, 부모 불일치, 재사용 중 설정 변경, 중복 종료, archive 도중 중단을 검사한다. 기존 run의 배정과 원본 기록을 비교한다.

</details>

<details>
<summary>U6 · 작업 평가를 생성·재사용·완료 검증에 적용</summary>

**수정 지점:** `src/delegation.rs`(신규), `src/plan.rs`, `src/session.rs::SessionSpec`, `src/engine.rs::ask`, `src/native/broker/dispatch.rs`, `src/native/{pool,reply}.rs`, `src/prompts.rs`, `src/render.rs`, `tests/delegation_policy.rs`(신규).

**입력:** 신규 `TaskAssessment`는 unit/role·계약 digest·요구 역량·depth·위험 세 값·근거·topology를 갖는다. topology는 선행 작업, 결합도(`independent/shared`), 공유 자원 ID, writer 소유권, 분리 가능성을 기록한다.
**출력:** 신규 `DelegationDecision`은 route, 모델·effort, lane, 평가/카탈로그/관찰 digest, 검증 의무와 reason code를 갖는다. route는 `worker/coordinator/replan/wait`다.

- 선행 조건 검사 → 역할과 작업의 역량 요구 합성 → 위험·공유 상태에 따른 route → 가용 후보 선택 → 검증 의무 확정 순서로 처리한다. 요구 수준은 역량별 최댓값, depth는 더 깊은 값을 사용한다.
- 결합도는 단순 파일 개수로 계산하지 않는다. 부모의 근거와 독립 검토를 사용하고, 실행 시 겹친 write 범위·같은 변경 가능 자원·writer 충돌을 추가 검사한다.
- 기존 worker의 모델·effort를 유지하며 요구 충족 여부를 매번 평가한다. 부족하면 적격 부모, 부모도 부족하면 `wait`다. 독립 검토 역할은 Critic/Auditor lane을 유지한다.
- 고위험 작업은 적격 작성자 배정, 작성 depth 최소 focused, 검토 depth deep과 복구 검증을 요구한다. 수행 가능한 격리 검증이 마련될 때까지 실제 변경은 대기한다.
- 입력 누락은 `assessment_missing`, 관찰 만료는 `host_stale`, 가용성 문제는 `model_unavailable`, 고정 배정 부족은 `bound_capability_insufficient`, 공유 상태는 `shared_state`로 설명한다.
- 등록·completion은 현재 dispatch의 판정 digest와 identity·모델·effort를 검사한다. 실패 이후 재시도도 같은 run 예산과 독립성을 유지한다.

**검증:** [배정 판정표](delegation.md#작업-분해와-위임-개선)의 6개 사례에 부모 역량 부족·독립 검토자 부재·slot 부족·지원 effort 불일치를 추가한다. 기대 route뿐 아니라 최종 native dispatch와 재사용 결과를 확인한다.

</details>

<details>
<summary>U7 · 전체 경로의 인수 검증</summary>

**수정 지점:** `tests/delegation_flow.rs`(신규), `tests/common/mod.rs`, 기존 native·lifecycle·stage-transition 테스트, 루트 `tests/gates.sh`, `scripts/set-version.py`의 버전 관리 대상, `README.md`, `plugins/hwahap/{ARCHITECTURE,OPERATIONS,USAGE}.md`.

- T01–T04는 U1, T05–T08은 U2, T09–T10은 U4, T11–T14는 U3/U4, T15는 U2/U4, T16–T17은 U3/U7에서 담당한다. 각 테스트 이름에 시나리오 ID를 넣어 추적한다.
- 일반 PLAN·direct BUILD·approved-plan 가져오기마다 유효 계약 1건과 거부 계약 1건을 실행한다. 이어서 ADJUST → 재검증 → 최종 전체 테스트 → 양측 PR 검토 → SHIP의 결속을 확인한다.
- native fixture는 실제 요청의 모델·effort·identity·판정 digest를 검증한다. 실제 모델 품질과 비용은 별도의 실행 관측값으로 표시한다.
- 최종 후보에 대해 fmt, Clippy, 전체 Cargo 테스트, 버전 일치, 정적 게이트를 실행한다. 실제 실패가 발생하면 수정한 후보에서 관련 검사와 최종 검증을 갱신한다.
- 배포 준비 버전은 0.1.1이다. 출시 작업은 완성된 후보와 별도로 진행한다. 실행에 사용한 0.1.0 바이너리와 해당 run의 기록은 보존한다.

</details>

전용 테스트 명령: `cargo test --locked --manifest-path plugins/hwahap/runtime/Cargo.toml --test <전용 테스트 이름>`.
완료 조건은 테스트 파일 존재·실제 시나리오 실행·검사 통과다. 공통 fixture는 실제 Git 저장소와 `Sessions`/`gh` 대역을 사용한다.
각 작업의 구현과 회귀 검증을 함께 커밋하고, 200줄 체크포인트에서는 해당 작업의 변경만 정리한다.
