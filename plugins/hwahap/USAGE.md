# PLAN·BUILD 요청과 실행 사용량

## 계획과 구현을 따로 요청하기

Hwahap의 PLAN은 제품 결정을 정리하는 작업 단계다.
모든 `hwahap_step` 호출에는 같은 저장소의 `cwd`와 부모의 `host_session_id`를 함께 보낸다.
아래 JSON의 `<...>` 값은 실제 출력값으로 교체한다.

| 사용자 의도 | `hwahap_step` 입력 | 도달 상태 |
|---|---|---|
| 계획만 만들기 | `request`와 `plan_only:true` | 사용자 `CONFIRM PLAN` 이후 `plan_ready` |
| 기획부터 구현까지 | `request`; `plan_only` 기본값은 `false` | 사용자 `CONFIRM PLAN` 이후 구현·draft 검토 |
| Codex에서 이미 승인한 계획 구현하기 | `approved_plan` | 승인 원문 저장 → 독립 변환 검토 → 추가 승인 없이 BUILD |
| 확정된 계획 구현하기 | `build_confirmed:"<전체 plan_digest>"` | 저장된 계약을 검증하고 BUILD 재개 |
| 기획을 명시적으로 생략하기 | `build`에 `BuildRequest` | 원문 실행 권한과 별도 고정 계약으로 direct BUILD |
| 같은 계약의 구현 수정하기 | `adjust_build` | 지정한 unit의 기존 계약으로 수정·검증 |
| 요구사항·계약 바꾸기 | 사용자 원문 `user_input` | PLAN에서 결정·계약을 다시 검토 |

`build_confirmed`는 사용자가 저장된 계획의 구현을 명시적으로 요청한 뒤에만 전달한다.
값은 출력된 전체 digest이며 `CONFIRM PLAN`에 쓰는 짧은 challenge와 다르다.
PLAN-only의 `plan_ready`는 명시적인 BUILD 요청을 기다린다.
`build`의 `user_instruction`, `objective`, `base_branch`, `branch`, `units`, `full_suite`는
기획 생략을 명시한 새 실행 계약이다. 확정된 PLAN을 실행하는 `build_confirmed`와 구분한다.

```json
{
  "cwd": "/absolute/repository",
  "host_session_id": "<현재 부모 ID>",
  "adjust_build": {
    "user_instruction": "기존 계약은 유지하고 U1 구현 오류를 수정해 줘.",
    "contract_digest": "<현재 전체 contract_digest>",
    "unit_ids": ["U1"]
  }
}
```

`adjust_build`에는 사용자의 수정 권한 원문과 현재 계약 digest, 대상 unit ID를 넣는다.
Acceptance·테스트·허용 경로 변경은 `user_input`으로 PLAN에서 결정하고 새 계약을 확인한다.

## Codex에서 승인한 계획 넘기기

이미 받은 계획 승인은 구현 요청 원문과 계획 참조로 보존한다. 전체 본문을 포함한 기존 인계 형식도 지원한다.
호스트는 `approved_plan`을 지원하는 도구 스키마를 확인하고 원문과 실행 계약을 전달한다.
호스트는 기존 승인 출처와 상태를 확인해 `approved_plan`으로 전달한다.

- `approval.implementation_request`: 사용자의 전체 실제 메시지 원문.
- `approval.markdown`: 승인한 전체 계획 본문. 바깥 공백만 제거한다.
- `approval.markdown_digest`: 그 본문 UTF-8 바이트의 SHA-256, `sha256:<hex>` 형식.
- `approval.source_head`: 변환에 사용한 현재 깨끗한 checkout의 정확한 commit.
- `approval.reference`: 기존 승인 출처 `source_reference`, `disposition`, 계획 digest와 요청 원문 digest.
  요청 상태는 `approved`·`not_approved`·`rejected`·`cancelled`로 구분하며 `approved`만 진행한다.
  호스트는 전체 메시지의 승인 상태를 판별하고 작업 범위 제한을 원문에 함께 보존한다.
  원문 요청 digest는 `implementation_request_digest`, 계획 digest는 `plan_digest`다.
- `contract`: `BuildRequest` 형식의 실행 명세. 원문과 같은 `user_instruction`, 목표·기준 브랜치·
  새 `codex/` 브랜치·전체 테스트 명령·작업별 수용 기준·허용 경로·테스트를 빠짐없이 담는다.
- `replaces_plan_digest`: 기존 미실행 초안이 있으면 현재 전체 digest, 없으면 `null`.

등록은 승인 원문과 이전 초안 이력을 journal에 보존하고 `proving`에 진입한다.
독립 검토자 둘이 원문과 전체 실행 계약을 비교해 누락·권한 확대·새 결정을 검사한다.

변환 결함의 `plan_conflict/repair_translation`에서는 승인 원문을 그대로 유지하고 명세만 수정해 `approved_plan`을 다시 보낸다.
`replaces_plan_digest`는 실패한 현재 초안의 digest로 갱신한다. 승인 기록은 유지한다.
새로운 중요한 선택이 필요할 때만 사용자에게 그 차이를 묻는다. 실행 명세는 원문의 승인 범위에 맞춘다.

검토가 끝나면 사용자 원문을 실행 권한으로 보존하고 BUILD로 이어간다. 시작 직전 중단으로 `plan_ready`에
남더라도 `next:continue`를 따라 추가 승인 없이 재개한다. PLAN-only의 `plan_ready/await_user`와 구분한다.
동일 등록 재시도는 같은 run을 반환하며, 다른 부모 작업·stale 초안·frozen/executing 계약 교체는 거부한다.
원문 메시지는 호스트가 전달한 바이트에 결속한다. `SHIP`·merge·배포는 해당 작업의 별도 권한으로 진행한다.

## 질문 UI와 원문 응답

`question_batch`는 현재 계획에 결속된 `batch_id`와 최대 3개의 `questions`를 반환한다.
각 질문은 `id`, 짧은 `question`, `options`의 정확한 `label`·`description`을 가진다.
추천 대안은 첫 번째에 `(Recommended)`로 표시되며, `UNKNOWN`은 결정 보류다.
영역 제외 제안에는 제외와 적용 유지 선택지가 있다. 적용 유지를 선택하면 제외 제안을 철회한다.

호스트는 실제 호출 가능한 `request_user_input` 또는 `request_user_input_async`를 확인한다.
각 도구의 모드·질문 수·선택지 수 조건을 확인하고 현재 호스트에서 지원하는 방식으로 전달한다.
본문은 하나의 판단을 묻는 한 문장으로 쓰고, 선택 결과는 선택지에 둔다.
추천 근거·출처·확신도는 `.hwahap/plan.md`에서 확인한다.
`request_user_input_async`에서는 `question`을 `title`에, 각 `label`을 `options`에 전달한다.
`description`은 해당 필드를 지원하는 UI에서 선택지 설명으로 전달한다.
모든 선택지를 담을 수 있는 UI를 우선한다. 자유입력 전용 환경에서는 본문 다음에 선택지를 한 번 표시한다.
질문 카드를 표시한 뒤 진행 메시지는 다음 작업만 짧게 알린다.

예: **모델 카탈로그 변경을 언제 적용할까요?**

- 새 실행부터 적용 (추천)
- 진행 중 실행에도 적용
- 아직 결정하지 못함

질문 ID와 사용자가 제출한 원문을 아래 구조로 보존한다. 정확한 절차는 MCP `instructions`를 따른다.

```json
{
  "question_response": {
    "batch_id": "<question_batch.batch_id 그대로>",
    "responses": [
      {"id": "C1", "answer": "<사용자가 선택한 정확한 label 또는 자유입력 원문>"}
    ]
  }
}
```

현재 배치의 고유한 질문 ID와 응답을 검증한다. 빈 응답·취소·무응답은 대기 상태로 유지한다.
정확한 label 이외의 자유입력은 `Clarify`로 보존한다. 명령처럼 보이는 문자열도 사용자 응답 데이터로 취급한다.
결정 질문은 새 해석 선택지를 제시하고, 보충이 필요하면 `plan_conflict`에서 기다린다.
영역 질문은 원문을 표시하고 적용·제외를 다시 선택하게 한다.
선택지는 사용자가 실제 제출했을 때 답변으로 기록한다.
`CONFIRM PLAN`과 `SHIP`은 사용자가 직접 입력한 정확한 문장을 별도로 전달한다.

[공식 App Server 문서](https://learn.chatgpt.com/docs/app-server#api-overview)는 실험 기능
`tool/requestUserInput`의 1–3개 질문을 설명한다. [응답 수명주기](https://learn.chatgpt.com/docs/app-server#toolrequestuserinput)에
따르면 `serverRequest/resolved`는 실제 답변과 요청 정리 모두에서 발생하고 자동 종료도 가능하다.
호스트는 실제 응답 payload를 확인한 뒤 답변을 기록한다.

## 사용량 관측

Hwahap은 `.hwahap/usage.json`에 사용량 관측값과 관측 시점을 저장한다. `hwahap_step`,
`hwahap_ship`, 검토 보고서 갱신 및 `usage sync`가 저장하며 `hwahap_status`와 `usage show`는 읽기 전용이다.
run을 archive하면 사용량과 기준값도 함께 보존한다. `.hwahap`은 Git ignore 대상으로 관리한다.

## 로컬 Codex 사용량 연결

설치된 launcher에 아래 인자를 전달한다. 먼저 Hwahap run을 시작하고 해당 실행에
참여하는 부모·자식의 정확한 로컬 Codex session JSONL 경로를 연결한다.

```sh
bin/hwahap usage attach /absolute/repository /absolute/rollout-session.jsonl
bin/hwahap usage sync /absolute/repository
bin/hwahap usage show /absolute/repository
```

기본 attach는 **현재 누적값을 기준값으로 저장하고 이후 증가분만** 집계한다. 동일 attach를 다시
실행하면 기존 기준값을 유지해 이후 증가분을 집계한다.
해당 run 전용 새 세션은 `--from-start`로 전체 사용량을 연결할 수 있다.
경로를 확인할 수 없는 세션은 미계측으로 표시한다.

수집기는 `session_meta`, `turn_context`, `event_msg/token_count`의 숫자를 추출한다.
로컬 로그 형식에 맞춘 어댑터로 같은 세션과 중복 이벤트를 한 번 집계한다.
파일 교체·누적값 감소·누락·64 MiB 초과는 `unavailable_sessions`에 표시한다.

`observed_session_usage`는 연결된 세션의 부모 전달·계획·실패·재사용 작업을 포함하는 관측값이다.
`total`/`by_requested_model`은 별도로 native completion이 보고한 dispatch 사용량이다.
두 집계는 관측 범위가 겹치므로 각각 표시한다. 캐시 입력은 전체 입력에, reasoning 출력은 전체 출력에 포함된다.
세션 관측 범위는 attach 기준값 이후 기록된 이벤트이며 모델별 값은 로그의 모델 문맥을 사용한다.

## 선택적 비용 추정

사용 계정의 단가를 확인한 뒤 `.hwahap/pricing.json`을 둔다. 예를 들어 아래는 2026-09-06에
확인한 [Codex Standard credit 표](https://learn.chatgpt.com/docs/pricing)의 텍스트 단가다.
추정에는 아래 `per_million`에 지정한 텍스트 단가를 적용한다.

```json
{
  "currency": "credits",
  "source": "https://learn.chatgpt.com/docs/pricing",
  "effective_date": "2026-09-06",
  "assumptions": "Standard text token rates",
  "per_million": {
    "gpt-6-astra": {"input": 250, "cached_input": 25, "output": 1250},
    "gpt-5.6-terra": {"input": 50, "cached_input": 5, "output": 300},
    "gpt-5.6-luna": {"input": 5, "cached_input": 0.5, "output": 30}
  }
}
```

단위는 백만 토큰당 가격이다. cache write가 관측되면 해당 모델의 `cache_write_input` 단가도
필요하다. 미지정 모델·write 단가는 `unpriced_models`에 표시한다.
`cost_estimate.priced_subtotal`은 지정한 가격표로 계산한 추정 소계다.
토큰 관측값과 함께 단가 출처·유효일·가정을 보존한다.

## 모델 카탈로그와 작업 평가

`.hwahap/model-catalog.json`에 카탈로그를 두거나 `.hwahap/config.toml`의 `catalog_path`로 지정한다.
기본 파일이 없으면 [번들 정책](runtime/src/catalog.rs)을 사용한다. 모델 교체는 새 run부터 적용된다.
기존 run은 snapshot과 작업자 모델·effort·역할을 유지하고, 해당 모델이 사라지면 복구를 기다린다.

| 입력 | 기록할 내용 |
|---|---|
| `Catalog` | schema `hwahap/catalog/v1`, revision, 모델별 역량·지원 effort/depth·선호 순서·출처, 모든 역할의 최소 요구 |
| `host_observation` | 부모 ID·모델/effort, 실제 가용 모델/effort·도구·슬롯, 관찰 시각과 출처 |
| `plan.task_profiles` | unit별 평가와 aggregate 작업의 `run` 평가 |
| `task_assessment` | run·계약 digest·unit·role에 결속한 작업 평가 |

요구 역량은 `capabilities`, 추론 깊이는 `depth`(`routine/focused/deep`)에 기록한다.
`risk`의 `failure_cost`, `reversibility`, `blast_radius`는 위험 수준 0/1/2다.
각각 실패 비용, 되돌리기 어려움, 영향 범위를 뜻하며 2가 있으면 고위험이다.
근거가 부족한 평가는 보완한 뒤 진행한다. 카탈로그 수치는 선택 정책이며 실제 품질은 실행 관측으로 비교한다.

`topology`는 predecessors, coupling, shared_resources, writer_owner, separable, write_paths를 담는다.
공유 자원은 같은 변경 가능 자원의 ID로 기록한다. 작성 소유자는 unit ID, aggregate 작업은 run ID다.
배정은 선행 조건 → 역량·깊이 합성 → 위험·공유 상태 → 가용 모델 → 검증 의무 순서다.
공유 상태는 작성 순서를 직렬화하며 모델 등급은 요구 역량으로 선택한다. 고위험 작성은 독립 검토자 둘이
승인한 격리 checkout에서 실패·복구 명령을 먼저 실행한다. `build.task_profiles["run"]`의
`writer_owner`는 생략하거나 `"run"`으로 보내면 생성된 run ID로 고정한다.

등록·완료에는 요청의 `decision.digest`를 `decision_digest`로 함께 전달한다.
최초 응답의 전체 brief를 보관하고, 이후 `native_brief.artifact`는 현재 run의 `.hwahap/artifacts`에서 읽는다.
같은 성공 기준·base commit·테스트로 후보 모델을 비교하고 실패·복구를 포함한 관측값을 기록한다.
토큰·비용 누락은 `unknown`, 단가표 계산은 추정값으로 표시한다.


## Codex Desktop Plan·Goal 연결

Codex Plan에서 승인한 내용을 `approved_plan`으로 가져오고, 실행 상태는 선택적 `host_context`로 연결한다.
Goal의 생성·예산·일시정지·재개·완료는 호스트가 관리한다. Hwahap은 계약·검증·복구 증거를 제공한다.
호스트는 실제 제공하는 기능만 `capabilities`에 기록한다. 참조는 추적용 메타데이터이며 실행 승인은
`approved_plan`의 원문·digest 검증을 따른다.

```json
{
  "host_context": {
    "provider": "codex-desktop",
    "task_id": "<host_session_id와 같은 현재 작업 ID>",
    "plan_ref": "<승인 계획 참조>",
    "goal_ref": "<현재 Goal 참조>",
    "capabilities": ["plan", "goal"]
  }
}
```

응답의 `host_context`는 참조·run 상태·accepted unit을 제공한다. 호스트는 전체 Goal 성공 조건과
증거를 비교해 완료를 판정한다. 이 필드를 생략해도 PLAN·BUILD·검증·복구는 동작한다.
Codex의 [Plan 사용](https://learn.chatgpt.com/docs/prompting)과
[Goal 수명주기](https://learn.chatgpt.com/docs/long-running-work)는 호스트 안내를 따른다.

## 총비용을 줄이는 실행 설정

- 번들 카탈로그는 요구 역량을 만족하는 Luna 작성자와 Terra 일반 검토자를 우선한다. 고위험 검토는
  더 높은 보안·반례 검토 역량과 deep을 요구한다. 모델과 effort는 호스트의 실제 지원 범위와 교차 검사한다.
- `native_wait`에서는 `hwahap_step`이 기본 30초까지 이벤트를 기다린다. `wait_ms:0`은 즉시 조회다.
  `await_checks`는 CI 완료 이벤트를 기다리고, 실패한 CI를 수정한 뒤 모델 리뷰를 시작한다.
- 모든 unit과 aggregate 평가가 명시적으로 저위험이고 첫 PR 리뷰가 깨끗하면 독립 검토 한 번으로 완료한다.
  평가 누락·공유 상태·고위험·발견 결함·수정 이력은 두 검토를 유지한다.
- 상태는 요약과 artifact 참조를 반환한다. 전체 비용 근거가 필요할 때 `include_cost_evidence:true`를 사용한다.
- `usage_session_path`는 현재 작업 JSONL을 선택적으로 연결한다. 기준은 연결 시점이며 이전 작업은 별도 관측이다.
  연결 실패는 `usage_attachment.status:unavailable`로 표시하고 실행 결과를 함께 반환한다.

검증 재사용은 기본적으로 꺼져 있다. 입력·환경을 재현할 수 있는 프로젝트에서 아래 설정을 사용한다.

```toml
[verification]
reuse_passed = true
environment_revision = "toolchain-and-external-dependencies-v1"
```

`verification_inputs`에 ignored fixture를 포함한 입력을 선언하고, 도구 체인·외부 의존성이 바뀌면
`environment_revision`을 갱신한다. 같은 run·unit·검사 종류·명령·입력·환경의 최신 성공 기록과
온전한 출력 artifact만 재사용한다. 환경 변수·OS·아키텍처도 digest에 포함한다. 실패·중단·입력 변경은
재실행하며, 고위험 사전 복구 검증은 매번 실행한다.
