# Hwahap 운영 절차

Hwahap은 구현 계획을 확정하거나 그 계약을 구현해 draft PR까지 진행한다.
run은 요청 하나의 실행 기록이며, unit은 계획 안에서 구현·검증·검토하는 작업 단위다.

`범위·성공 기준 → PLAN·CONFIRM → plan_ready 또는 BUILD → draft → 공격·방어·수정 → ADJUST 또는 SHIP`

이 문서는 `hwahap/v5` 운영 기준이다. [설치](README.md), [호스트 실행 기록](PLATFORM.md),
[요청 형식](USAGE.md)을 참고한다. 도구 호출·등록·완료 전달은 실행 중인
[MCP instructions](runtime/src/mcp.rs)와 반환된 `next`·`message`를 따른다.

## 1. 시작 전: 범위와 실행 환경

사용자는 원하는 결과와 중요한 제약을 전달한다. 부모는 저장소를 조사하고 작업을 분해한다.

| 항목 | 시작 요청에 넣을 내용 |
|---|---|
| 목표 | 누가 어떤 상황에서 무엇을 할 수 있어야 하는가 |
| 성공 기준 | 관찰 가능한 동작, 검증 명령, 성능·호환성 기준 |
| 범위 | 구현할 기능과 보존할 파일·데이터·기존 동작 |
| 환경 | 대상 저장소, 기준 브랜치, 기술 스택과 실행 환경 |
| 전달 | 한 draft PR 또는 독립적으로 검토·통합할 여러 단계 |

호스트는 Git 상태, 기준 브랜치·commit, 기존 run, MCP 연결과 native 도구를 확인한다.
Commit 작성자·커미터는 실행 worktree의 Git 설정을 사용하며 저장소 설정 다음에 사용자 설정을 읽는다.
`user.name`과 `user.email`을 설정하고, 이메일은
[GitHub 계정에 연결된 주소](https://docs.github.com/en/account-and-profile/how-tos/email-preferences/setting-your-commit-email-address)를 사용한다.
Push 인증은 [Git credential](https://git-scm.com/docs/gitcredentials) 설정으로 관리한다.
같은 작업의 진행·수정에는 기존 active run을 사용한다.
소스에서 확인할 사실은 저장소를 조사하고 제품 동작·기술 선택·성공 기준은 사용자와 결정한다.

Hwahap 자체를 수정할 때는 실행 commit과 바이너리 경로·해시를 기록한다.
검증한 release로 실행하면서 소스 checkout에서 후보를 검증한다.
실행과 pending을 종료한 뒤 검증한 스킬·release를 함께 설치하고 MCP 연결을 새로 연다.
기존 run 재개에는 현재 schema와 필수 리뷰 필드를 갖춘 기록을 사용한다.

- 계획만 요청: `request`와 `plan_only:true`로 시작해 `plan_ready`에서 계획을 전달한다.
- 확정 계획 구현: 사용자의 구현 요청을 받은 뒤 전체 계획 digest를 `build_confirmed`로 전달한다.
- 계획부터 구현: `plan_only:false` 또는 기본값을 사용해 계획 확인 후 BUILD로 이어간다.
- Codex 승인 계획: 구현 요청 원문·기존 승인 참조와 전체 계획을 `approved_plan`으로 전달한다.
  기존 승인을 보존하고 실행 계약으로 변환한 내용을 독립 검토한다.
- 기획 생략: 사용자가 명시한 실행 권한으로 `build`를 전달한다.

Direct BUILD 필드는 다음과 같다.

- `user_instruction`: 사용자의 기획 생략·구현 권한 원문.
- `objective`, `base_branch`, `branch`: 목표, `origin/<base_branch>` 기준, 새 `codex/` 작업 브랜치.
- `units`: `title`, `acceptance`, `paths`, `test_command`를 가진 순차 작업 목록.
- `full_suite`: PR 게시와 수정 후 실행할 통합 검증 명령.

실행기는 원격 기준 commit·범위·추적 관계를 검증하고 계약을 고정한다.
작업 평가와 카탈로그로 작성자를 선택하고 독립 Critic·Auditor가 검토한다.
동일 BUILD 재전송은 같은 run을 반환한다. 초기 worktree 생성 중 단절은 저장된 계약과 Git 상태를 확인해 재개한다.

## 2. PLAN: 결정하고 구현 계약을 확인하기

Worker는 저장소 사실을 조사하고 부모는 선택지·추천·구현 구조를 만든다.
[모델·effort 정책](ARCHITECTURE.md#5-모델effort-정책)에 따라 작성·검토 책임을 배정한다.
PLAN은 선행 조건이 해결된 질문 집합인 frontier를 계산하고 최대 3개씩 UI에 전달한다.
답변을 받은 뒤 `Refining`에서 파급 효과를 검토해 다음 라운드를 구성한다.
이 방식은 [grilling](https://github.com/mattpocock/skills/blob/main/skills/productivity/grilling/SKILL.md)을 참고했다.

호스트는 질문·대안·추천 근거 전체를 표시하고 사용자 원문을 `question_response`로 전달한다.
현재 배치·질문 ID·정확한 label에 결속된 응답을 채택한다.
`UNKNOWN`은 결정 보류, 자유입력은 `Clarify`로 보존한다.
새 해석 선택지를 사용자에게 제시하며 보충이 필요하면 `plan_conflict`에서 답을 기다린다.
영역 질문은 원문과 함께 적용·제외를 다시 선택하게 한다.
`CONFIRM PLAN`과 `SHIP`은 사용자가 직접 입력한 정확한 문장으로 전달한다.

새 run은 source commit을 기록하고 근거 경로·줄이 해당 commit의 추적 파일에 존재하는지 검사한다.
Source가 바뀐 재계획은 사실·답변·영역 제외를 새 기준에서 확인한다.
계획은 요구사항, 관찰 가능한 acceptance, unit 의존관계, unit별 테스트와 full suite를 담는다.
독립된 Auditor의 ColdConsumer와 Critic의 PlanCritic이 계약을 검토한다.
검토 결과는 현재 `review_digest`에 결속한다. 기계 검증과 두 검토 후 사용자는 `.hwahap/plan.md`를 확인한다.
승인하려면 현재 출력된 `CONFIRM PLAN <challenge>`를 직접 입력한다. 계획이 바뀌면 새 challenge를 사용한다.

## 3. BUILD: 순차 구현과 draft

계획 승인, 확정 계획의 `build_confirmed`, direct BUILD 권한 중 해당 경로에 따라 구현을 시작한다.
호스트는 승인 범위 안에서 Hwahap이 지정한 작업을 진행한다.

1. 의존관계 순서로 실행할 unit을 선택한다.
2. 배정된 Worker 또는 부모가 허용 경로에서 첫 구현을 수행한다.
3. 실행기가 실제 변경 경로와 테스트 종료 상태를 검사하고 Critic이 검토한다.
4. 실패하면 적격 부모가 한 번 재작업한다. 재실패는 근거를 기록하고 중단한다.
5. 통과한 변경을 commit하고 모든 unit과 full suite가 완료되면 draft PR을 게시한다.
6. Critic의 공격 보고서와 별도 Auditor의 방어 판정을 받는다.
7. 확인된 결함은 부모가 수정하고 고정 테스트·full suite를 통과한 뒤 같은 PR에 push해 새 head를 재검토한다.

두 검토자는 읽기 전용으로 작업한다. 보고서는 PR URL·head SHA·계약 digest와 독립된 작업자 ID에 결속한다.
방어팀은 각 공격 항목을 `confirmed/refuted/unresolved`로 판정하고 근거를 남긴다.
미해결 항목이나 실행 예산 소진은 PR과 근거를 보존한 채 중단한다.

### 보안 검토

두 검토자는 변경된 입력에서 중요한 작업까지 경로를 추적하고 재현·반박과 추가 경로를 독립 검토한다.
`security.threat_model`에는 보호 자산, 공격자가 제어하는 입력, 신뢰 경계와 환경 가정을 기록한다.

| 필수 영역 | 검사 대상 |
|---|---|
| `authorization` | 인증·권한·소유권·tenant·저장소 경계 |
| `untrusted_input` | 명령·경로·프롬프트 주입, symlink, 역직렬화, 네트워크 요청 |
| `secrets` | 자격증명, 로그·산출물의 정보 노출, 암호 처리 |
| `supply_chain` | 의존성, 빌드·업데이트 출처, 실행 hook |
| `state_integrity` | 재전송, 경쟁 조건, 변조, 오류 처리와 작업 순서 |
| `resource_exhaustion` | 입출력 크기, 시간, 재시도·spawn, 취소 |

양쪽 보고서에 여섯 영역을 각각 한 번 포함한다. `checked`는 수행한 검사,
`not_applicable`은 적용 판단의 코드 근거, `blocked`는 장애 원인과 다음 검사를 기록한다.
`evidence`에는 파일·검사 명령·관찰 결과를, `finding_ids`에는 관련 결함 ID를 연결한다.
보안 finding은 공격자의 전제·제어 범위, 경계·영향, 최소 재현 또는 소스 경로,
기대·관찰 결과와 회귀 검사 제안을 담는다.

런타임은 필수 필드·영역·참조·근거를 검사한다. `blocked`는 검토와 SHIP을 중단한다.
확인된 결함은 수정 후 새 head에서 두 팀이 재검토한다.
테스트 명령 출력은 stdout·stderr 각각 1 MiB까지 수집하고 초과 시 실패 처리와 종료를 수행한다.
시간 제한·취소 시 Unix의 같은 process group도 종료한다.
재현에는 임시 fixture와 가짜 데이터를 사용한다. 상세 근거는 로컬에, 공개 PR에는 민감정보를 정리한 요약을 남긴다.

검토 영역은 [OWASP WSTG](https://owasp.org/www-project-web-security-testing-guide/stable/4-Web_Application_Security_Testing/),
[Threat Modeling](https://cheatsheetseries.owasp.org/cheatsheets/Threat_Modeling_Cheat_Sheet.html),
[NIST SSDF 1.1](https://csrc.nist.gov/pubs/sp/800/218/final)을 참고해 구현했다.
진행 중 변경 요청은 현재 run의 계획·수정 흐름에 전달한다. 상태는 `hwahap_status`로 확인한다.

## 4. ADJUST와 SHIP

기존 계약의 구현 수정은 `adjust_build`에 사용자 원문, 현재 `contract_digest`, 대상 `unit_ids`를 담는다.
Acceptance·테스트·허용 경로 변경은 `user_input`으로 PLAN을 열고 새 계약을 확인한다.
유효한 accepted unit은 유지하고 변경된 unit과 의존 unit을 다시 수행한다.
계획·통과 기록은 같은 run에서, 에이전트는 같은 저장소·부모 pool에서 재사용한다.

사용자가 현재 `SHIP <challenge>`를 입력하면 계약 결속, 현재 PR head의 두 독립 검토,
결함 해결과 필수 checks를 확인한 뒤 draft를 ready로 전환한다.
이후 코드 소유자 리뷰·merge·배포는 각 작업의 승인과 운영 절차에 따라 진행한다.

검토 재개는 같은 `cwd`·`host_session_id`와 `recheck_pr:true`로 요청한다.
기존 draft URL·브랜치·깨끗한 worktree·원격 head·계약을 확인하고 full suite부터 진행한다.
완료된 검토는 새 round를 열며 단절된 검토의 저장 보고서·누적 수정 횟수는 유지한다.
현재 schema의 필수 진행 기록과 보고서 필드를 사용한다.
진행 중 repair의 예정 commit 기록을 이용해 commit·push 사이 단절을 같은 PR에서 복구한다.
예정 commit 저장 전의 dirty 변경은 사용자가 검토·보존·정리한다.
PR 갱신은 저장된 URL을 사용한다. 수정 push 후 이전 head가 조회되면 최대 4회, 500ms 간격으로 재조회한다.
다른 head 또는 지속되는 불일치는 관찰·예정 SHA를 기록하고 중단한다.

## 5. 큰 기능을 여러 단계로 진행하기

부모가 전체 목표·범위·인터페이스·성공 기준을 정리하고 검증 가능한 단계와 의존관계를 제안한다.
사용자는 중요한 범위·기술 결정과 단계 경계를 확인한다.

| 구성 | 운영 방법 |
|---|---|
| 한 run·한 PR | 하나의 계약에 순차 unit을 두고 같은 run의 ADJUST로 수정한다. |
| 여러 run·여러 PR | 전체 계약·단계표를 남기고 각 run에 범위·성공 기준·선행 commit을 명시한다. |

단계표에는 API·상태 형식·호환성, 통합 검증과 완료 조건을 기록한다.
다음 run은 문서와 실제 선행 commit을 읽는다. 일반 run은 PLAN 확인, direct BUILD는 명시적 실행 권한,
각 draft의 ready 전환은 해당 `SHIP`을 사용한다.

단계별 결과 예시:

1. 계획·승인·상태 저장과 재시작.
2. native 실행·생성 실패·단절 복구.
3. 구현·독립 검토·draft PR.
4. 계약·데이터·설치 호환성과 실제 호스트 통합 검증.

선행 코드가 필요하면 이전 PR을 merge하고 checkout·기준 commit을 갱신한다.
브랜치에 직접 의존할 때는 그 기반을 명시한다.
종료된 run의 작업 트리에 남은 변경과 ignored 파일을 검토·보존·정리한 뒤 새 run을 시작한다.
작업 트리 정리 후 기존 run을 archive하며, 새로운 direct BUILD는 새 checkout에서 시작한다.

## 6. 중단 상태별 대응

| 상태 | 의미와 다음 행동 |
|---|---|
| `plan_ready/continue` | 승인 계획의 검토 완료. 시작 장애를 해결하고 기존 구현 권한으로 재개한다. |
| `plan_ready/await_user` | 계획 결과를 전달한다. 구현 요청을 받으면 전체 digest를 `build_confirmed`로 전달한다. |
| `plan_conflict/repair_translation` | 승인 원문을 유지하고 실행 명세를 수정해 현재 초안 digest와 함께 `approved_plan`으로 전달한다. |
| 그 외 `plan_conflict` | 원문·충돌을 읽고 사용자 입력으로 PLAN의 결정·계약을 재검토한다. |
| `blocked` | 원인·테스트·Git 상태를 확인하고 남은 실행을 종료한다. 원인 해결 후 새 요청은 별도 run으로 시작한다. |
| `native_paused` | spawn 실패를 저장한 상태. run·plan·accepted unit을 유지하고 새 호스트 회복 관찰로 재개한다. |
| `native_stop` | 해당 dispatch의 에이전트와 명령을 찾아 종료를 확인한 뒤 복구한다. |
| 입력 오류 | 현재 단계·계약·challenge에 맞는 사용자 원문을 전달한다. |

복구에는 저장된 상태와 정상 MCP 절차를 사용한다.
`native_paused`의 새 관찰은 해당 재시도에 한 번 사용하며 재개 요청도 64회 기본 예산에 포함한다.
저장된 run 단계에서 재개하므로 진행 중인 unit이나 역할을 다시 수행할 수 있다.

pool은 같은 run·부모의 Worker·Critic·Auditor ID와 모델·effort를 유지한다.
첫 생성 이후 같은 ID에 follow-up하며 최초 슬롯 부족이나 유지한 자식의 소실은 실행 중단으로 처리한다.
기본 연결 제한은 180초이며 등록 후 수행 제한은 별도로 180초다. soft 목표는 사실·계약·plan/unit 리뷰·진단 60초,
구현·재작업·최종 리뷰 120초, 나머지 계획 역할 90초이며 hard 값으로 상한을 둔다.
최대 30초 이벤트 대기를 사용하고 `native-timing-<id>.json`에 시각·크기·종료 사유를 기록한다.
연결 대기와 등록 후 수행 시간을 구분한다. Git·GitHub·테스트 명령은 각각의 명령 실행 절차를 따른다.
Unit 재시작은 소유한 후보를 백업하고 복원한다. 실패 기록과 시도 횟수를 유지하며 빌드 캐시는 worktree 밖에 둔다.
[호스트 한도](https://learn.chatgpt.com/docs/config-file/config-reference)와
[스레드 관리](https://learn.chatgpt.com/docs/agent-configuration/subagents)는 공식 설정을 참고한다.

## 7. 완료 보고

- run ID, 계획 revision·digest, 기준 commit, 작업 브랜치·PR URL, 실행 버전.
- 변경 동작, 테스트 명령·결과, 독립 리뷰, 실행 환경과 다음 확인 항목.
- 통제된 fixture 테스트와 실제 호스트 실행의 출처.
- 요청·완료·재작업·중단 수, 사용량 관측 범위와 가격표 기반 추정값.
- 현재 단계: `plan_ready`, draft, ready, merged, deployed 또는 실사용 검증 완료.

[사용량 계측](USAGE.md)에 따라 부모·자식 세션을 등록하고 미계측은 `unknown`으로 표시한다.
운영 구현은 [상태 기계](runtime/src/engine.rs), [계획 계약](runtime/src/plan.rs),
[native 복구](runtime/src/native/host.rs), [PR 검사](runtime/src/forge.rs), [비용 집계](runtime/src/cost.rs)에 있다.
