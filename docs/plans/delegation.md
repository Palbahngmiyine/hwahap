# 작업 분해와 위임 개선

상태: 작업 명세 작성 완료. [구현 파일·입출력 계약·테스트·인계 조건](delegation-work-items.md)을 기준으로 착수한다.
모델 카탈로그 변경은 새 실행부터 적용한다. 진행 중 실행은 기록된 모델·effort·역할을 유지하고, 제공 중단 시 복구 대기한다.

| 작업 | 완료 결과 | 핵심 검증 | 선행 작업 |
| --- | --- | --- | --- |
| U1 검증 연결 | 각 작업의 완료 조건을 그 작업의 테스트가 모두 검증 | 다른 작업의 테스트로 누락을 가린 반례 거부 | — |
| U2 구조 수정 | 설계 보완은 수정·재검토로, 사용자 선택은 짧은 질문으로 처리 | 구조 결함 수정, 승인 변경 차단, 반복 예산 | U1, U3 |
| U3 검증 기록 | 현재 코드와 실행 결과를 결속하고 중단 후 복구 | 저장 중단·중복 결과·오래된 증거 | U1 |
| U4 재검증 | 완료된 동일 계약은 테스트 재실행으로 확인 | 구현 호출·빈 커밋 없이 완료 | U2, U3 |
| U5 모델 카탈로그 | 교체 가능한 모델·effort 정보와 실행별 배정 보존 | 새 실행 적용, 기존 실행 유지, 제공 종료 | U3 |
| U6 위임 정책 | 역량·작업 관계·추론 깊이·위험으로 실제 배정 | 위험·난도·공유 상태별 예상 배정 | U2, U4, U5 |
| U7 통합 검증 | 모든 진입점·복구·최종 검토를 연결하고 문서화 | T01–T17, 모델 교체, 실제 dispatch 계약 | U6 |

실행 순서: **U1 → U3 → U2 → U4 → U5 → U6 → U7**. 각 작업은 구현과 해당 회귀 테스트를 함께 완료한다.

<details>
<summary>U1 · 작업별 완료 조건과 테스트 연결</summary>

- 조건: 비-probe 작업의 acceptance와 테스트가 각각 존재하고, 자체 테스트들의 acceptance 합집합이 작업의 모든 acceptance를 포함한다.
- 반례: U1=[A1,A2], T1(U1)=[A1], U2=[A2], T2(U2)=[A2]는 U1/A2 누락으로 거부한다. 여러 자체 테스트의 합집합과 작업 간 공통 acceptance는 허용한다.
- 적용: 일반 PLAN, direct BUILD, approved-plan 가져오기, 계약 수정의 공통 validator에서 검사한다. 기존 ID·참조·순환 검사와 오류 정렬을 보존한다.
- 수정: `validate.rs`, 계약 입력 회귀 테스트. 검증: `unit_verification`의 T01–T04.

</details>

<details>
<summary>U2 · 검토 지적의 분류와 구조 수정</summary>

- 검토자는 지적마다 `id`, `kind`, 대상, 근거, 기대 결과를 기록한다. 종류는 `choice`, `fact`, `structure`, `blocker`이며 부모가 경로를 제안하고 런타임이 계약 변경을 검사한다.
- 혼합 지적은 하위 지적으로 나눠 원래 ID와 연결한다. 사실 보완 → 사용자 선택 → 구조 수정 → 독립 재검토 순서로 의존성을 해결한다. blocker는 필요한 근거·권한이 확보될 때까지 대기한다.
- 구조 수정은 승인 전 unit·테스트 연결·의존성·경로 배치를 다룬다. 사용자 선택의 의미·acceptance·명령·권한 변화는 계약 변경 경로로 보낸다. 승인된 계약의 변경도 기존 승인 절차를 따른다.
- 분할·병합 후보와 이유는 `review_digest`에 결속된 검토 증거로 저장한다. 이유만 바뀌면 unit fingerprint는 유지한다. 계약 요소 변화는 검토를 갱신한다.
- 구조 수정 예산은 run 전체 3회다. 재분할과 프로세스 재시작에도 누적하고, 미해결 지적은 다음 검토까지 보존한다. 한 결과·불변조건을 보존하는 여러 파일의 변경은 유효한 unit이다.
- 수정: `plan.rs`, `proposal.rs`, `prompts.rs`, `engine.rs`, `state.rs`. 검증: `decomposition_review`의 T05–T08, T15.

</details>

<details>
<summary>U3 · 검증 증거와 저장 중단 복구</summary>

- `VerificationEvidence`: `schema`, `id`, `run_id`, `unit_id`, `kind`, `attempt`, `fingerprint`, `command`, `cwd`, `head`, `tree`, `status`, `exit_code`, `output_ref`, `output_digest`를 저장한다.
- kind는 `unit`, `revalidation`, `final_unit`, `full_suite`다. full_suite는 run 범위와 전체 계약 digest에 결속한다. output은 `.hwahap/artifacts` 내부 파일과 SHA-256으로 참조한다.
- fingerprint는 기존 unit·acceptance·requirement·선택 의미·테스트 명령을 포함한다. 테스트 코드 변경은 HEAD/tree 비교로 판별한다. 경로·의존성·실행 환경 정책 변경도 계약 비교에 포함한다.
- 순서: 실행 의도를 journal에 기록 → 명령 실행 → 출력 파일 저장 → 완료 이벤트 기록 → snapshot 반영. journal을 권위로 삼고 뒤처진 snapshot은 재구성한다.
- 완료 이벤트 전 중단은 결과 미확정으로 복구하고 소유 프로세스 종료 확인 후 재검증한다. 완료 이벤트 후 중단은 출력 해시까지 검증해 재구성한다. snapshot 선행·출력 유실·충돌 completion은 복구 대기다.
- 성공은 종료 코드 0과 실행 전후 동일 코드·계약·테스트 입력이 모두 필요하다. 커밋 전에는 base HEAD·fingerprint·예정 tree, 완료 시에는 실제 commit tree에 결속한다. ignored 테스트 입력은 선언 목록으로 해시를 기록하고 빌드 산출물과 구분한다.
- 마지막 후보에서 모든 비-probe unit의 고정 테스트와 full_suite를 실행한다. 각 결과를 같은 후보에 결속한 뒤 PR 검토와 SHIP에서 검사한다.
- 저장 schema는 `hwahap/v5`로 올린다. v4 active run은 변경 전에 버전 불일치를 반환하고 원본을 보존한다. 기존 실행은 0.1.0으로 처리하고 새 schema는 새 실행에 사용한다.
- 수정: `state.rs`, `engine.rs`, 검증 실행·PR/SHIP 모듈. 검증: `verification_evidence`의 T11–T14, T16–T17.

</details>

<details>
<summary>U4 · 구현 완료 기록을 이용한 재검증</summary>

- 자격은 같은 run의 런타임 검증을 거친 구현 완료 기록, 같은 fingerprint, 직접 수정 의무의 부재로 판정한다. 작업자의 완료 주장과 분리해 저장한다.
- ADJUST의 명시된 unit ID는 직접 수정 대상이다. 확정 PR 지적은 파일을 소유하는 모든 unit에 연결한다. 연결 결과가 비거나 불명확하면 영향 검토로 대상을 확정한다.
- 직접 대상과 의존성으로 무효화된 작업을 별도 기록한다. 직접 대상·새 작업·계약 변경 작업은 구현 경로, 나머지 자격 충족 작업은 재검증 경로로 보낸다.
- 재검증은 구현 dispatch와 커밋 없이 고정 테스트를 실행한다. 실패·timeout·입력 변화는 실패 기록과 기존 시도 예산을 유지한다.
- U4에서는 재검증마다 Critic의 현재 후보에 결속된 영향 검토를 적용한다. U6에서 단일 작업 내부·낮은 위험으로 확인된 경우 고정 테스트 결과로 판정하는 경로를 추가한다.
- 수정: `engine.rs`, `engine/adjust_build.rs`, PR repair, `state.rs`. 검증: `unit_revalidation`의 T09–T12, T15.

</details>

<details>
<summary>U5 · 교체 가능한 카탈로그와 호스트 관찰</summary>

- 설정: `.hwahap/config.toml`의 `catalog_path`가 가리키는 JSON. 기본 경로는 `.hwahap/model-catalog.json`이다. 번들 기본 카탈로그는 모델별 선언 출처를 가진 시작 설정이다.
- 카탈로그 필드: `schema`, `revision`, `models`, `role_requirements`. 모델별로 `id`, 역량별 수준 0–3, `efforts`(이름·지원 depth·선호 순서), 모델 선호 순서, 근거 출처를 요구한다.
- 역량 키는 `repository_analysis`, `implementation`, `cross_module_reasoning`, `adversarial_review`, `security_review`다. 0은 미확인, 1은 제한된 작업, 2는 통합 작업, 3은 복잡한 교차 영역 작업의 선언 수준이다.
- 호스트는 `hwahap_step.host_observation`으로 `host_session_id`, `observed_at`, 출처, 모델별 지원 effort·도구, 부모 모델·effort, 생성 가능한 slot 수를 전달한다. 사용자 설정과 호스트 관찰을 별도로 기록한다.
- 관찰은 dispatch 전 300초 이내여야 한다. 누락·만료·미확인 역량·선언과 가용성 충돌 시 이유를 표시하고 복구 대기한다. 실제 spawn 실패는 기존 stop/recovery 절차를 따른다.
- run 시작 때 카탈로그 내용·revision·해시를 snapshot에 고정한다. 이후에는 가용성 관찰만 갱신한다. 변경된 설정은 다음 run에 적용한다.
- pool은 run과 부모 작업에 결속한다. 이전 작업자의 작업 종료와 slot 해제는 별도로 확인한다. 새 run은 가용 slot에서 새 배정을 사용하고, 진행 중 lane은 같은 identity·모델·effort·역할로 재사용한다.
- 영구 제공 종료: `hwahap_step.abandon`에 run ID와 사용자 종료 지시를 받는다. pending 작업의 종료 확인 후 기록·출력·pool 결속을 archive하고 종료한다. 새 request는 새 카탈로그와 새 승인 기록으로 시작한다.
- 기존 `profiles` 설정은 구체적인 새 카탈로그 변환 안내를 반환한다. 기존 schema의 실행과 새 schema의 실행은 각각 대응 런타임으로 처리한다.
- 수정: `config.rs`, `profile.rs`, `native.rs`, `native/pool.rs`, `native/host.rs`, `mcp.rs`, `state.rs`. 검증: `capability_catalog`의 카탈로그 교체·호스트 만료·중복 종료·종료 중 중단 복구.

</details>

<details>
<summary>U6 · 실제 위임 판정표</summary>

부모가 작업 요구와 근거를 작성하고 독립 검토자가 확인한다. 런타임은 아래 순서로 같은 입력에 같은 판정을 만든다.

1. 권한·선행 작업 완료·writer 소유권·검토 독립성을 확인한다. 위반은 대기 또는 계약 변경 경로다.
2. 역량별 최소 수준과 추론 깊이를 확인한다. depth는 `routine`(명확한 절차), `focused`(여러 조건의 통합), `deep`(모호한 원인·교차 영역 추론)이다.
3. 위험 세 축을 각각 평가한다. failure cost는 국소 수정/통합 재작업/데이터·보안·서비스 손실, reversibility는 검증된 자동 복구/수동 복구/복구 불가, blast radius는 격리 작업/공유 저장소/외부 사용자로 나눠 0/1/2를 부여한다.
4. 한 축이라도 2면 고위험이다. 평균으로 낮추지 않는다. 미확인 값은 사실 보완 후 판정한다.
5. 고위험 또는 강한 공유 상태는 부모가 처리한다. 나머지는 필요한 역량·depth·도구를 충족하는 worker에게 위임한다. 의존 작업은 선행 결과가 준비된 뒤 순차 실행한다.
6. 최초 worker는 첫 실제 배정 작업의 요구로 선택한다. 후보는 카탈로그와 현재 호스트 관찰의 교집합이며, 충족 후보를 모델 선호 순서 → 모델 ID → effort 선호 순서 → effort 이름으로 정렬한다.
7. 이후 고정 worker의 역량이 부족하면 조건을 충족하는 부모로 보낸다. 부모도 부족하거나 독립 검토자를 확보할 수 없으면 복구 대기한다. 실행 중 부모 모델 변경은 새 실행 절차를 따른다.
8. 고위험은 작성 depth를 최소 `focused`, 검토 depth를 `deep`으로 요구한다. Critic의 영향·권한 검토, Auditor의 복구 절차 검토, 승인된 격리 환경의 실패·복구 재현 결과를 변경 전 확인한다.

| 입력 사례 | 예상 배정 | 검증 |
| --- | --- | --- |
| 낮은 난도·낮은 위험·독립 작업 | 조건을 충족한 worker / routine | 고정 테스트와 기존 독립 검토 |
| 낮은 난도·높은 위험 | 적격 작성자 / 최소 focused | deep 독립 검토와 실패·복구 재현 |
| 높은 난도·낮은 위험 | deep을 지원하는 고정 worker, 부족하면 부모 | 고정 테스트와 기존 독립 검토 |
| 공유 저장소 상태를 함께 수정 | 부모 / 작업에 필요한 depth | 변경 소유권과 영향 검토 |
| 미확인 역량 또는 만료된 호스트 정보 | 사실 보완·복구 대기 | 새 관찰 후 동일 배정 조건 재확인 |
| 고정 모델 제공 중단 | 기존 배정 보존·복구 대기 | 제공 복구 또는 명시적 종료 후 새 실행 |

- 판정 기록: 평가 입력·근거·카탈로그 해시·호스트 관찰·요구 역량·depth·위험 세 값·route·모델·effort·검증 의무·reason code를 dispatch와 completion에 결속한다.
- 현재 부모의 모델·effort와 요구 조건을 비교한다. 카탈로그 선언, 호스트 보고, 요청 모델, 실제 실행에서 관측된 모델은 각 출처로 표시한다.
- 수정: task assessment 타입, `profile.rs`, `session.rs`, `native.rs`, `native/pool.rs`, dispatch 생성·등록·완료 검증. 검증: `delegation_policy`의 위 판정표와 slot·예산·identity 충돌.

</details>

<details>
<summary>U7 · 전체 흐름과 문서 검증</summary>

- 참고 작업 「개발 작업 단위 분해 조사」(`6a9d6c17-883c-83e8-814e-1736c7f6019f`)의 최종 제안 T01–T17을 각 테스트 파일의 시나리오 ID와 연결한다.
- 일반 PLAN·direct BUILD·approved-plan 가져오기에서 공통 validator, ADJUST·재검증·PR repair·SHIP에서 현재 증거 소비를 확인한다.
- 임의 모델 ID와 서로 다른 effort 이름의 fixture로 카탈로그 교체를 검증한다. 등록·재사용·중단·복구의 실제 native dispatch 필드를 확인한다.
- 패키지 목표 버전은 `0.1.1`, 저장 schema는 `hwahap/v5`다. 버전 스크립트·manifest·게이트·CI·README·운영 문서를 일치시킨다.
- UI는 한 문장 질문, 짧은 선택 결과, 별도 상세 근거를 유지한다. 실패 시 사용자가 취할 다음 행동을 한 문장으로 표시한다.
- 구현 범위는 순차 실행·단일 writer·독립 검토·MCP 3개·로컬 STDIO다. 병렬 스케줄러, 증분 테스트 캐시, 자동 probe는 후속 과제로 둔다.
- 검증: `delegation_flow`, 전체 Cargo 테스트, Clippy, fmt, `python3 tests/versions.py`, `bash tests/gates.sh`.
- fixture 검증과 실제 모델 실행 결과는 구분한다. 비용 비교는 성공률·재시도·시간·실제 사용량이 함께 관측된 실행을 대상으로 한다.

</details>

각 전용 테스트 명령은 저장소 루트에서 `cargo test --locked --manifest-path plugins/hwahap/runtime/Cargo.toml --test <테스트 이름>`이다.
기존 테스트를 확장할 때도 위 시나리오 ID를 유지한다. 필드의 생성부터 실행·저장·복구·최종 승인까지 소비하는 경로를 함께 검증한다.

설계 근거: [검증기](../../plugins/hwahap/runtime/src/validate.rs), [계약 fingerprint](../../plugins/hwahap/runtime/src/plan.rs), [journal 복구](../../plugins/hwahap/runtime/src/state.rs), [작업자 결속](../../plugins/hwahap/runtime/src/native/pool.rs), [질문 UI](../../plugins/hwahap/USAGE.md).
