import 'package:flutter_bloc/flutter_bloc.dart';

part 'search_filter_state.dart';

class SearchFilterCubit extends Cubit<SearchFilterState> {
  SearchFilterCubit() : super(const SearchFilterState(language: null, decade: null, duet: null));

  String? get language => state.language;
  String? get decade => state.decade;
  bool? get duet => state.duet;

  set language(String? language) {
    emit(SearchFilterState(language: language, decade: decade, duet: duet));
  }

  set decade(String? decade) {
    emit(SearchFilterState(language: language, decade: decade, duet: duet));
  }

  set duet(bool? duet) {
    emit(SearchFilterState(language: language, decade: decade, duet: duet));
  }

  String? queryString(String? text) {
    if (text == null && state.language == null && state.decade == null && state.duet == null) {
      return null;
    }

    return [
      if (text != null) text,
      if (state.language != null) 'language:"${state.language!}"',
      if (state.decade != null) 'year:[${state.decade!}]',
      if (state.duet != null) 'duet:${state.duet}',
    ].join(' AND ');
  }
}
